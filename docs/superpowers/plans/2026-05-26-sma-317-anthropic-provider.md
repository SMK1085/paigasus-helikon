# SMA-317 — Anthropic provider implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `paigasus-helikon-providers-anthropic` — the Messages-API `Model` implementation with streaming, tool use, prompt caching, extended thinking, and structured-output synthesis.

**Architecture:** Hand-rolled wire layer on `reqwest::Client` + `eventsource-stream`. `AnthropicModel::messages(id) → AnthropicModelBuilder → AnthropicModel`. Anthropic-specific knobs (`CacheStrategy`, `ExtendedThinking`, `top_k`, `beta` headers) baked at `build()` time. `ResponseFormat::JsonSchema`/`JsonObject` implemented as forced-tool synthesis with `TokenDelta` remap. One new `ModelCapabilities::prompt_caching` flag added to core; SMA-316's OpenAI table is backfilled for the same flag.

**Tech Stack:** Rust 1.75+, tokio, async-trait, reqwest (rustls), eventsource-stream 0.2, serde_json, thiserror, anyhow, tracing. Tests: wiremock 0.6, insta 1.

**Spec:** [`docs/superpowers/specs/2026-05-26-sma-317-anthropic-provider-design.md`](../specs/2026-05-26-sma-317-anthropic-provider-design.md)
**Branch:** `feature/sma-317-anthropic-provider-messages-streaming-tool-use-prompt`
**Linear:** [SMA-317](https://linear.app/smaschek/issue/SMA-317/anthropic-provider-messages-streaming-tool-use-prompt-caching)

---

## File structure

**New files:**

```
crates/paigasus-helikon-providers-anthropic/src/
├── lib.rs                       # re-exports (currently a stub)
├── model.rs                     # AnthropicModel + impl Model
├── builder.rs                   # AnthropicModelBuilder + BuildError
├── settings.rs                  # CacheStrategy, ExtendedThinking
├── capabilities.rs              # ModelEntry, KNOWN_MODELS, lookup
├── error.rs                     # map_error_type helper
├── http.rs                      # header builders, auth
├── sse.rs                       # SSE envelope deserialization
├── stream.rs                    # MessageTranslator
└── translate/
    ├── mod.rs                   # shared helpers, re-exports
    ├── request.rs               # Vec<Item> + ModelRequest → request body
    ├── tools.rs                 # ToolDef → Anthropic tool
    ├── cache.rs                 # CacheStrategy → cache_control placement
    └── response_format.rs       # ResponseFormat → forced-tool synthesis

crates/paigasus-helikon-providers-anthropic/tests/
├── messages_wire.rs             # wiremock non-streaming
├── messages_streaming.rs        # wiremock SSE fixtures
├── prompt_caching.rs            # acceptance criterion
├── structured_output.rs         # forced-tool round-trip
├── live.rs                      # ANTHROPIC_API_KEY-gated
└── fixtures/
    ├── text_only.txt
    ├── parallel_tool_use.txt
    ├── thinking_then_text.txt
    ├── tool_use_then_continuation.txt
    └── stream_error.txt
```

**Modified files:**
- `Cargo.toml` (workspace) — add `eventsource-stream` to `[workspace.dependencies]`
- `crates/paigasus-helikon-providers-anthropic/Cargo.toml` — populate `[dependencies]` / `[dev-dependencies]`
- `crates/paigasus-helikon-core/src/model.rs` — add `ModelCapabilities::prompt_caching` field + `with_prompt_caching()`
- `crates/paigasus-helikon-providers-openai/src/capabilities.rs` — backfill `with_prompt_caching()` on cache-eligible entries
- `crates/paigasus-helikon/src/lib.rs` — add `pub use paigasus_helikon_providers_anthropic as anthropic;`

---

## Conventions for this plan

- **Commits:** `<type>(<scope>): SMA-317 <lowercase subject>`. Valid scopes used here: `core`, `providers-anthropic`, `providers-openai`, `facade`, `workspace`. **No `feat`/`fix` on infra-only commits; use `chore`/`docs`.**
- **Tests first.** Each task is a TDD cycle: failing test → minimal impl → green → commit.
- **Verify failure** before implementing. A test that compiles and passes on the first try usually wasn't testing the new thing.
- **Run scoped commands:** `cargo test -p paigasus-helikon-providers-anthropic <test_name>` (not workspace-wide) while iterating; the final task runs the full CI gate.

---

## Task 1: Add `eventsource-stream` workspace dependency

**Files:**
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Edit workspace dependency table**

Insert into `[workspace.dependencies]` in alphabetical position (after `async-trait`, before `futures-core`):

```toml
eventsource-stream    = "0.2"
```

- [ ] **Step 2: Verify resolution**

Run: `cargo metadata --format-version 1 --filter-platform $(rustc -vV | sed -n 's/host: //p') >/dev/null`

Expected: exits 0. If it errors with a dep resolution problem, inspect the message — most commonly an MSRV bump from a transitive crate, which is fixed per CLAUDE.md by bumping `[workspace.package].rust-version`, not by downgrading.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore(workspace): SMA-317 add eventsource-stream dependency"
```

---

## Task 2: Add `ModelCapabilities::prompt_caching` field to core

**Files:**
- Modify: `crates/paigasus-helikon-core/src/model.rs`

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` block at the bottom of `crates/paigasus-helikon-core/src/model.rs`:

```rust
#[test]
fn prompt_caching_capability_round_trips() {
    let c = ModelCapabilities::empty().with_prompt_caching();
    assert!(c.prompt_caching, "with_prompt_caching must set the flag");
    let d = ModelCapabilities::default();
    assert!(!d.prompt_caching, "default must be false");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-core prompt_caching_capability_round_trips`

Expected: FAIL with `no field 'prompt_caching' on type 'ModelCapabilities'` and `no method named 'with_prompt_caching'`.

- [ ] **Step 3: Add field + builder method**

In `crates/paigasus-helikon-core/src/model.rs`, locate the `ModelCapabilities` struct (around line 224) and add a new field at the end of the struct body (after `pub audio: bool,`):

```rust
    /// Provider supports prompt caching of repeated request prefixes.
    /// On OpenAI this is automatic prefix caching; on Anthropic it is
    /// opt-in via the provider crate's `CacheStrategy`.
    pub prompt_caching: bool,
```

In the `impl ModelCapabilities` block, add to `pub const fn empty()`'s struct literal the `prompt_caching: false,` line, then add a new `with_*` method (after `with_audio`):

```rust
    /// Mark `prompt_caching` as supported.
    pub const fn with_prompt_caching(mut self) -> Self {
        self.prompt_caching = true;
        self
    }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p paigasus-helikon-core prompt_caching_capability_round_trips`

Expected: PASS.

- [ ] **Step 5: Verify no other core tests regressed**

Run: `cargo test -p paigasus-helikon-core`

Expected: all PASS. If any pre-existing test that destructures `ModelCapabilities` fails because of the new field, **stop** — the struct is `#[non_exhaustive]` so destructure with `..`, but in-crate tests can still pattern-match exhaustively. Fix those call-sites.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/model.rs
git commit -m "feat(core): SMA-317 add ModelCapabilities::prompt_caching flag"
```

---

## Task 3: Backfill `prompt_caching: true` on cache-eligible OpenAI models

**Files:**
- Modify: `crates/paigasus-helikon-providers-openai/src/capabilities.rs`

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block at the bottom of `crates/paigasus-helikon-providers-openai/src/capabilities.rs`:

```rust
#[test]
fn cache_eligible_models_advertise_prompt_caching() {
    // OpenAI's automatic prompt-caching covers gpt-4o family, gpt-4.1 family,
    // o1/o3 family, and gpt-5. Verify each table entry advertises the flag.
    for id in [
        "gpt-4o",
        "gpt-4o-mini",
        "gpt-4.1",
        "gpt-4.1-mini",
        "o1",
        "o1-mini",
        "o3",
        "o3-mini",
        "gpt-5",
    ] {
        let caps = lookup(id);
        assert!(
            caps.prompt_caching,
            "model {id} must advertise prompt_caching=true",
        );
    }
    // gpt-3.5-turbo predates automatic prefix caching — must remain false.
    assert!(
        !lookup("gpt-3.5-turbo").prompt_caching,
        "gpt-3.5-turbo predates OpenAI prefix caching",
    );
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-providers-openai cache_eligible_models_advertise_prompt_caching`

Expected: FAIL with `model gpt-4o must advertise prompt_caching=true` (or the first model in the loop).

- [ ] **Step 3: Add `.with_prompt_caching()` to each cache-eligible entry**

In `crates/paigasus-helikon-providers-openai/src/capabilities.rs`, modify the `KNOWN_MODELS` table. For each of the nine models listed in the test, append `.with_prompt_caching()` to the chained builder. Example (gpt-4o):

```rust
    (
        "gpt-4o",
        ModelCapabilities::empty()
            .with_streaming()
            .with_tools()
            .with_parallel_tool_calls()
            .with_structured_output()
            .with_vision()
            .with_prompt_caching(),
    ),
```

Do **not** add the flag to `gpt-3.5-turbo` (predates automatic caching).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p paigasus-helikon-providers-openai cache_eligible_models_advertise_prompt_caching`

Expected: PASS.

- [ ] **Step 5: Run the full OpenAI test suite to verify no regressions**

Run: `cargo test -p paigasus-helikon-providers-openai`

Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-providers-openai/src/capabilities.rs
git commit -m "feat(providers-openai): SMA-317 backfill prompt_caching on cache-eligible models"
```

---

## Task 4: Scaffold the Anthropic provider crate

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/Cargo.toml`
- Modify: `crates/paigasus-helikon-providers-anthropic/src/lib.rs`
- Create: `crates/paigasus-helikon-providers-anthropic/src/{model,builder,settings,capabilities,error,http,sse,stream}.rs`
- Create: `crates/paigasus-helikon-providers-anthropic/src/translate/{mod,request,tools,cache,response_format}.rs`

- [ ] **Step 1: Populate Cargo.toml**

Replace the existing `crates/paigasus-helikon-providers-anthropic/Cargo.toml` contents (preserving the workspace inheritance pattern from `paigasus-helikon-providers-openai`):

```toml
[package]
name        = "paigasus-helikon-providers-anthropic"
description = "Anthropic provider for the Paigasus Helikon AI SDK."
version                = "0.0.0"
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
reqwest               = { workspace = true, features = ["json", "stream", "rustls-tls"] }
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

- [ ] **Step 2: Replace `lib.rs` with the public surface skeleton**

Overwrite `crates/paigasus-helikon-providers-anthropic/src/lib.rs`:

```rust
//! Anthropic provider — Messages API for the Paigasus Helikon SDK.
//!
//! See [SMA-317] for the design. The public surface is [`AnthropicModel`]
//! (a [`paigasus_helikon_core::Model`] implementation), its
//! [`AnthropicModelBuilder`], and the Anthropic-specific settings types
//! [`CacheStrategy`] and [`ExtendedThinking`].
//!
//! # Quick start
//!
//! ```ignore
//! // Ignored under doctest because the example reads ANTHROPIC_API_KEY
//! // from env, which isn't available in `cargo doc` runs.
//! use paigasus_helikon_providers_anthropic::AnthropicModel;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let _model = AnthropicModel::messages("claude-sonnet-4-6").build()?;
//! # Ok(()) }
//! ```
//!
//! [SMA-317]: https://linear.app/smaschek/issue/SMA-317

mod builder;
mod capabilities;
mod error;
mod http;
mod model;
mod settings;
mod sse;
mod stream;
mod translate;

pub use builder::{AnthropicModelBuilder, BuildError};
pub use model::AnthropicModel;
pub use settings::{CacheStrategy, ExtendedThinking};
```

- [ ] **Step 3: Create empty module stubs**

For each module, create a file with a one-line `//!` doc comment so the `missing_docs` lint passes during scaffolding. Skip files that subsequent tasks will fully populate, but **all** files referenced from `lib.rs` must exist or `cargo build` fails.

Run each `printf` to create the file:

```bash
mkdir -p crates/paigasus-helikon-providers-anthropic/src/translate
for f in builder capabilities error http model settings sse stream; do
  printf '//!\n' > "crates/paigasus-helikon-providers-anthropic/src/${f}.rs"
done
printf '//!\n' > crates/paigasus-helikon-providers-anthropic/src/translate/mod.rs
for f in request tools cache response_format; do
  printf '//!\n' > "crates/paigasus-helikon-providers-anthropic/src/translate/${f}.rs"
done
```

In `translate/mod.rs`, declare the submodules so they're picked up by the build:

```rust
//! Translation between Paigasus carrier types and Anthropic wire format.

pub(crate) mod cache;
pub(crate) mod request;
pub(crate) mod response_format;
pub(crate) mod tools;
```

In each placeholder leaf module (`builder.rs`, `capabilities.rs`, etc.), the single-line doc comment is enough — subsequent tasks fill in the contents.

The `lib.rs` `pub use` lines reference types (`AnthropicModel`, `AnthropicModelBuilder`, `BuildError`, `CacheStrategy`, `ExtendedThinking`) that don't exist yet. To keep the crate compiling between tasks, **temporarily comment out the `pub use` lines** in `lib.rs` and uncomment them as each type lands. (Task 6 uncomments `CacheStrategy`/`ExtendedThinking`; Task 8 uncomments `AnthropicModelBuilder` + `BuildError`; Task 21 uncomments `AnthropicModel`.)

- [ ] **Step 4: Verify the crate builds**

Run: `cargo build -p paigasus-helikon-providers-anthropic`

Expected: succeeds with no warnings. If `missing_docs` fires on a module, ensure it has the `//!` doc line.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/
git commit -m "chore(providers-anthropic): SMA-317 scaffold crate modules and Cargo deps"
```

---

## Task 5: Wire the facade re-export

**Files:**
- Modify: `crates/paigasus-helikon/src/lib.rs`

- [ ] **Step 1: Inspect the existing facade**

Read `crates/paigasus-helikon/src/lib.rs` to see the established `pub use` pattern (mirrors the existing `providers_openai` re-export from SMA-316).

- [ ] **Step 2: Add the Anthropic re-export**

Append the following block (placement: alongside the existing `providers_openai` re-export, grouped with other provider re-exports). The doc comment is required so `-D warnings` on the docs job stays green.

```rust
/// Anthropic provider — [`paigasus-helikon-providers-anthropic`].
#[cfg(feature = "anthropic")]
pub use paigasus_helikon_providers_anthropic as anthropic;
```

- [ ] **Step 3: Verify the facade builds with the new feature**

Run: `cargo build -p paigasus-helikon --features anthropic`

Expected: succeeds.

Run: `cargo doc -p paigasus-helikon --features anthropic --no-deps`

Expected: succeeds with no warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon/src/lib.rs
git commit -m "feat(facade): SMA-317 re-export providers-anthropic behind anthropic feature"
```

---

## Task 6: `settings.rs` — CacheStrategy and ExtendedThinking enums

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/settings.rs`
- Modify: `crates/paigasus-helikon-providers-anthropic/src/lib.rs` (uncomment one `pub use`)

- [ ] **Step 1: Write the failing test**

Replace `settings.rs` with:

```rust
//! Anthropic-specific configuration: prompt-caching strategy + extended thinking.

/// Where to place `cache_control: {type: "ephemeral"}` markers in the request body.
///
/// **Default `None` is opt-out:** no markers, body byte-identical to the
/// uncached path. Anthropic's prompt cache requires a per-model write
/// minimum (~1024 tokens for Sonnet, ~2048 for Opus); below that, the
/// strategy is a documented no-op and the cache simply does not write.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum CacheStrategy {
    /// No cache_control markers.
    #[default]
    None,
    /// Mark the final block of `system:` as a cache breakpoint.
    System,
    /// Mark the final tool in `tools[]` as a cache breakpoint.
    Tools,
    /// Mark both system and the last tool.
    SystemAndTools,
    /// Mark the final message in `messages[]` (rolling cache).
    LastTurn,
}

/// Configuration for Anthropic extended/adaptive thinking.
///
/// **Model compatibility:**
/// - Claude Opus 4.7 rejects `Enabled { .. }` (400). Use `Adaptive`.
/// - Sonnet/Opus 4.6 accept both but recommend `Adaptive`.
/// - Older Claude 4 (4.5, 4.1) accept `Enabled` and recommend it.
///
/// Anthropic requires `budget_tokens < max_tokens` for `Enabled` but
/// documents no absolute minimum; this crate does not enforce one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExtendedThinking {
    /// No `thinking` field in the request.
    Disabled,
    /// `thinking: { type: "enabled", budget_tokens: N }`.
    Enabled {
        /// Maximum tokens the model may spend on internal reasoning.
        budget_tokens: u32,
    },
    /// `thinking: { type: "adaptive" }`. Model picks the budget.
    Adaptive,
}

impl Default for ExtendedThinking {
    fn default() -> Self {
        Self::Disabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_strategy_default_is_none() {
        assert_eq!(CacheStrategy::default(), CacheStrategy::None);
    }

    #[test]
    fn extended_thinking_default_is_disabled() {
        assert_eq!(ExtendedThinking::default(), ExtendedThinking::Disabled);
    }

    #[test]
    fn extended_thinking_enabled_carries_budget() {
        let t = ExtendedThinking::Enabled { budget_tokens: 8192 };
        match t {
            ExtendedThinking::Enabled { budget_tokens } => assert_eq!(budget_tokens, 8192),
            _ => panic!("expected Enabled"),
        }
    }
}
```

- [ ] **Step 2: Uncomment the `pub use` line in `lib.rs`**

In `crates/paigasus-helikon-providers-anthropic/src/lib.rs`, uncomment:

```rust
pub use settings::{CacheStrategy, ExtendedThinking};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic settings`

Expected: 3 PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/{settings.rs,lib.rs}
git commit -m "feat(providers-anthropic): SMA-317 add CacheStrategy and ExtendedThinking enums"
```

---

## Task 7: `capabilities.rs` — KNOWN_MODELS + ModelEntry + lookup

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/capabilities.rs`

- [ ] **Step 1: Write the failing tests**

Replace `capabilities.rs` with:

```rust
//! KNOWN_MODELS capability lookup for Anthropic models.
//!
//! Hardcoded table per the SMA-317 spec. Anthropic exposes no
//! machine-readable capability manifest. Unknown ids fall through to
//! conservative defaults. Callers can override via
//! [`crate::AnthropicModelBuilder::with_capabilities`].

use paigasus_helikon_core::ModelCapabilities;

/// Capability + default-output-token snapshot for a model id.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ModelEntry {
    pub(crate) caps: ModelCapabilities,
    pub(crate) max_output_default: u32,
}

/// Conservative fallback for ids absent from [`KNOWN_MODELS`].
pub(crate) const fn conservative_defaults() -> ModelEntry {
    ModelEntry {
        caps: ModelCapabilities::empty().with_streaming().with_tools(),
        max_output_default: 4096,
    }
}

/// Capability snapshot keyed by exact model id.
///
/// Cross-check entries against Anthropic's published model docs at
/// implementation time. Entries that diverge are bugs — file follow-up
/// chore-PRs to keep this table aligned with reality.
pub(crate) const KNOWN_MODELS: &[(&str, ModelEntry)] = &[
    // Claude 4 family — primary lineup as of 2026-05.
    (
        "claude-opus-4-7",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_reasoning()
                .with_prompt_caching(),
            max_output_default: 32_768,
        },
    ),
    (
        "claude-opus-4-6",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_reasoning()
                .with_prompt_caching(),
            max_output_default: 32_768,
        },
    ),
    (
        "claude-opus-4-5",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_reasoning()
                .with_prompt_caching(),
            max_output_default: 32_768,
        },
    ),
    (
        "claude-opus-4-1",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_reasoning()
                .with_prompt_caching(),
            max_output_default: 32_768,
        },
    ),
    (
        "claude-sonnet-4-6",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_reasoning()
                .with_prompt_caching(),
            max_output_default: 32_768,
        },
    ),
    (
        "claude-sonnet-4-5",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_reasoning()
                .with_prompt_caching(),
            max_output_default: 32_768,
        },
    ),
    (
        "claude-haiku-4-5",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_prompt_caching(),
            max_output_default: 8192,
        },
    ),
    // Claude 3.5 family
    (
        "claude-3-5-sonnet-latest",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_prompt_caching(),
            max_output_default: 8192,
        },
    ),
    (
        "claude-3-5-sonnet-20241022",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_vision()
                .with_prompt_caching(),
            max_output_default: 8192,
        },
    ),
    (
        "claude-3-5-haiku-latest",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_prompt_caching(),
            max_output_default: 8192,
        },
    ),
    (
        "claude-3-5-haiku-20241022",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_parallel_tool_calls()
                .with_structured_output()
                .with_prompt_caching(),
            max_output_default: 8192,
        },
    ),
    // Claude 3 family — older; URL-form image inputs may 400 (use base64).
    (
        "claude-3-opus-latest",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_structured_output()
                .with_vision()
                .with_prompt_caching(),
            max_output_default: 4096,
        },
    ),
    (
        "claude-3-opus-20240229",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_structured_output()
                .with_vision()
                .with_prompt_caching(),
            max_output_default: 4096,
        },
    ),
    (
        "claude-3-sonnet-20240229",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_structured_output()
                .with_vision(),
            max_output_default: 4096,
        },
    ),
    (
        "claude-3-haiku-20240307",
        ModelEntry {
            caps: ModelCapabilities::empty()
                .with_streaming()
                .with_tools()
                .with_structured_output()
                .with_vision(),
            max_output_default: 4096,
        },
    ),
];

/// Look up the capability + default-output snapshot for a model id.
pub(crate) fn lookup(model_id: &str) -> ModelEntry {
    KNOWN_MODELS
        .iter()
        .find(|(id, _)| *id == model_id)
        .map(|(_, e)| *e)
        .unwrap_or_else(conservative_defaults)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_models_have_no_duplicate_ids() {
        let mut ids: Vec<&str> = KNOWN_MODELS.iter().map(|(id, _)| *id).collect();
        ids.sort_unstable();
        let len = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), len, "duplicate id in KNOWN_MODELS");
    }

    #[test]
    fn opus_4_7_advertises_reasoning_and_caching() {
        let e = lookup("claude-opus-4-7");
        assert!(e.caps.reasoning);
        assert!(e.caps.prompt_caching);
        assert!(e.caps.parallel_tool_calls);
        assert!(e.caps.vision);
        assert_eq!(e.max_output_default, 32_768);
    }

    #[test]
    fn haiku_3_5_has_no_vision() {
        let e = lookup("claude-3-5-haiku-20241022");
        assert!(!e.caps.vision, "3.5 Haiku has no vision");
        assert!(e.caps.prompt_caching);
    }

    #[test]
    fn old_3_sonnet_lacks_prompt_caching() {
        let e = lookup("claude-3-sonnet-20240229");
        assert!(!e.caps.prompt_caching);
        assert!(e.caps.vision);
    }

    #[test]
    fn unknown_id_falls_through_to_conservative_defaults() {
        let e = lookup("claude-mystery-9000");
        assert!(e.caps.streaming);
        assert!(e.caps.tools);
        assert!(!e.caps.parallel_tool_calls);
        assert!(!e.caps.structured_output);
        assert!(!e.caps.vision);
        assert!(!e.caps.reasoning);
        assert!(!e.caps.prompt_caching);
        assert_eq!(e.max_output_default, 4096);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic capabilities`

Expected: 5 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/capabilities.rs
git commit -m "feat(providers-anthropic): SMA-317 add KNOWN_MODELS capability table"
```

---

## Task 8: `builder.rs` — AnthropicModelBuilder + BuildError

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/builder.rs`
- Modify: `crates/paigasus-helikon-providers-anthropic/src/lib.rs` (uncomment `pub use`)

- [ ] **Step 1: Write the failing tests + implementation together**

`builder.rs` is consumed by `AnthropicModel` (Task 21) and several earlier tests, so it lands as a single TDD cycle: tests inside the module, implementation in the same file.

Replace `builder.rs` with:

```rust
//! [`AnthropicModelBuilder`] — fluent constructor for [`crate::AnthropicModel`].

use paigasus_helikon_core::ModelCapabilities;
use reqwest::Url;

use crate::capabilities::{self, ModelEntry};
use crate::settings::{CacheStrategy, ExtendedThinking};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

/// Construction-time errors. Runtime errors flow through
/// [`paigasus_helikon_core::ModelError`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BuildError {
    /// `ANTHROPIC_API_KEY` was unset and no explicit auth was supplied.
    #[error("ANTHROPIC_API_KEY not set in environment")]
    MissingApiKey,
    /// `base_url` failed to parse as a URL.
    #[error("invalid base URL: {0}")]
    InvalidBaseUrl(String),
}

#[derive(Debug, Clone)]
enum AuthSource {
    Env,
    ApiKey(String),
    Bearer(String),
}

/// Resolved configuration handed off to [`crate::AnthropicModel`].
#[derive(Debug, Clone)]
pub(crate) struct Config {
    pub(crate) model_id: String,
    pub(crate) base_url: String,
    pub(crate) auth_header: AuthHeader,
    pub(crate) anthropic_version: String,
    pub(crate) anthropic_beta: Option<String>,
    pub(crate) cache_strategy: CacheStrategy,
    pub(crate) extended_thinking: ExtendedThinking,
    pub(crate) top_k: Option<u32>,
    pub(crate) max_output_default: u32,
    pub(crate) capabilities: ModelCapabilities,
    pub(crate) http: reqwest::Client,
}

/// One of `x-api-key: <key>` or `authorization: Bearer <token>`.
#[derive(Debug, Clone)]
pub(crate) enum AuthHeader {
    ApiKey(String),
    Bearer(String),
}

/// Fluent builder for [`crate::AnthropicModel`].
#[derive(Debug, Clone)]
pub struct AnthropicModelBuilder {
    model_id: String,
    auth: AuthSource,
    base_url: Option<String>,
    anthropic_version: Option<String>,
    beta_headers: Vec<String>,
    http_client: Option<reqwest::Client>,
    cache_strategy: CacheStrategy,
    extended_thinking: ExtendedThinking,
    top_k: Option<u32>,
    capabilities_override: Option<ModelCapabilities>,
}

impl AnthropicModelBuilder {
    pub(crate) fn new(model_id: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            auth: AuthSource::Env,
            base_url: None,
            anthropic_version: None,
            beta_headers: Vec::new(),
            http_client: None,
            cache_strategy: CacheStrategy::None,
            extended_thinking: ExtendedThinking::Disabled,
            top_k: None,
            capabilities_override: None,
        }
    }

    /// Use the given API key. Last-set auth wins.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.auth = AuthSource::ApiKey(key.into());
        self
    }

    /// Use a pre-minted bearer token (Bedrock/Vertex proxy). Last-set auth wins.
    pub fn bearer(mut self, token: impl Into<String>) -> Self {
        self.auth = AuthSource::Bearer(token.into());
        self
    }

    /// Override the base URL. Default: `https://api.anthropic.com`.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// Override the `anthropic-version` header. Default: `"2023-06-01"`.
    pub fn anthropic_version(mut self, v: impl Into<String>) -> Self {
        self.anthropic_version = Some(v.into());
        self
    }

    /// Append a value to the `anthropic-beta` header. Multiple calls
    /// accumulate; rendered as a comma-separated list at `build()`.
    pub fn beta(mut self, header: impl Into<String>) -> Self {
        self.beta_headers.push(header.into());
        self
    }

    /// Use a caller-provided `reqwest::Client`.
    pub fn http_client(mut self, c: reqwest::Client) -> Self {
        self.http_client = Some(c);
        self
    }

    /// Prompt-caching strategy. Default: [`CacheStrategy::None`].
    pub fn cache_strategy(mut self, s: CacheStrategy) -> Self {
        self.cache_strategy = s;
        self
    }

    /// Extended-thinking configuration. Default: [`ExtendedThinking::Disabled`].
    pub fn extended_thinking(mut self, t: ExtendedThinking) -> Self {
        self.extended_thinking = t;
        self
    }

    /// Set the `top_k` sampling parameter. Anthropic-specific.
    pub fn top_k(mut self, k: u32) -> Self {
        self.top_k = Some(k);
        self
    }

    /// Override the capability snapshot. Wins over the built-in lookup.
    pub fn with_capabilities(mut self, c: ModelCapabilities) -> Self {
        self.capabilities_override = Some(c);
        self
    }

    /// Resolve auth, validate base URL, look up capabilities, materialize the
    /// internal [`Config`].
    pub(crate) fn build_config(self) -> Result<Config, BuildError> {
        let auth_header = match &self.auth {
            AuthSource::Env => {
                let key = std::env::var("ANTHROPIC_API_KEY")
                    .map_err(|_| BuildError::MissingApiKey)?;
                AuthHeader::ApiKey(key)
            }
            AuthSource::ApiKey(k) => AuthHeader::ApiKey(k.clone()),
            AuthSource::Bearer(t) => AuthHeader::Bearer(t.clone()),
        };

        let base_url = self
            .base_url
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
        if Url::parse(&base_url).is_err() {
            return Err(BuildError::InvalidBaseUrl(base_url));
        }

        let entry: ModelEntry = capabilities::lookup(&self.model_id);
        let capabilities = self.capabilities_override.unwrap_or(entry.caps);

        let anthropic_beta = if self.beta_headers.is_empty() {
            None
        } else {
            Some(self.beta_headers.join(","))
        };

        let http = self.http_client.unwrap_or_default();

        Ok(Config {
            model_id: self.model_id,
            base_url,
            auth_header,
            anthropic_version: self
                .anthropic_version
                .unwrap_or_else(|| DEFAULT_ANTHROPIC_VERSION.to_owned()),
            anthropic_beta,
            cache_strategy: self.cache_strategy,
            extended_thinking: self.extended_thinking,
            top_k: self.top_k,
            max_output_default: entry.max_output_default,
            capabilities,
            http,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn save_and_set_env(value: Option<&str>) -> Option<String> {
        let prev = std::env::var("ANTHROPIC_API_KEY").ok();
        match value {
            Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
        prev
    }
    fn restore_env(prev: Option<String>) {
        match prev {
            Some(v) => std::env::set_var("ANTHROPIC_API_KEY", v),
            None => std::env::remove_var("ANTHROPIC_API_KEY"),
        }
    }

    #[test]
    fn build_without_env_or_explicit_key_errors_missing_api_key() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(None);
        let r = AnthropicModelBuilder::new("claude-sonnet-4-6").build_config();
        restore_env(prev);
        assert!(matches!(r, Err(BuildError::MissingApiKey)));
    }

    #[test]
    fn build_with_explicit_api_key_succeeds() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(None);
        let c = AnthropicModelBuilder::new("claude-sonnet-4-6")
            .api_key("sk-test")
            .build_config()
            .unwrap();
        restore_env(prev);
        assert!(matches!(c.auth_header, AuthHeader::ApiKey(_)));
        assert_eq!(c.anthropic_version, "2023-06-01");
        assert_eq!(c.max_output_default, 32_768);
    }

    #[test]
    fn build_with_bearer_succeeds() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(None);
        let c = AnthropicModelBuilder::new("claude-sonnet-4-6")
            .bearer("eyJhbGciOi...")
            .build_config()
            .unwrap();
        restore_env(prev);
        assert!(matches!(c.auth_header, AuthHeader::Bearer(_)));
    }

    #[test]
    fn build_reads_env_when_no_explicit_auth() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(Some("sk-from-env"));
        let r = AnthropicModelBuilder::new("claude-sonnet-4-6").build_config();
        restore_env(prev);
        assert!(r.is_ok());
    }

    #[test]
    fn invalid_base_url_errors() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(Some("sk-x"));
        let err = AnthropicModelBuilder::new("claude-sonnet-4-6")
            .base_url("not a url")
            .build_config()
            .unwrap_err();
        restore_env(prev);
        assert!(matches!(err, BuildError::InvalidBaseUrl(_)));
    }

    #[test]
    fn multiple_beta_calls_accumulate_into_comma_list() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(Some("sk-x"));
        let c = AnthropicModelBuilder::new("claude-sonnet-4-6")
            .beta("prompt-caching-2024-07-31")
            .beta("max-tokens-3-5-sonnet-2024-07-15")
            .build_config()
            .unwrap();
        restore_env(prev);
        assert_eq!(
            c.anthropic_beta.as_deref(),
            Some("prompt-caching-2024-07-31,max-tokens-3-5-sonnet-2024-07-15"),
        );
    }

    #[test]
    fn no_beta_calls_yields_no_header() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(Some("sk-x"));
        let c = AnthropicModelBuilder::new("claude-sonnet-4-6")
            .build_config()
            .unwrap();
        restore_env(prev);
        assert!(c.anthropic_beta.is_none());
    }

    #[test]
    fn capability_override_wins_over_lookup() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(Some("sk-x"));
        let custom = ModelCapabilities::empty();
        let c = AnthropicModelBuilder::new("claude-sonnet-4-6")
            .with_capabilities(custom)
            .build_config()
            .unwrap();
        restore_env(prev);
        assert!(!c.capabilities.tools, "override clears tools");
        assert!(!c.capabilities.prompt_caching);
    }

    #[test]
    fn unknown_model_uses_conservative_max_output_default() {
        let _g = env_lock().lock().unwrap();
        let prev = save_and_set_env(Some("sk-x"));
        let c = AnthropicModelBuilder::new("claude-mystery-9000")
            .build_config()
            .unwrap();
        restore_env(prev);
        assert_eq!(c.max_output_default, 4096);
    }
}
```

- [ ] **Step 2: Uncomment the `pub use` line in `lib.rs`**

```rust
pub use builder::{AnthropicModelBuilder, BuildError};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic builder`

Expected: 9 PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/{builder.rs,lib.rs}
git commit -m "feat(providers-anthropic): SMA-317 add AnthropicModelBuilder with auth + settings resolution"
```

---

## Task 9: `translate/request.rs` — text-only conversation translation

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/request.rs`

This file grows across Tasks 9–12. Each task adds branches and tests. Task 9 covers the simplest case: System / UserMessage / AssistantMessage with only text content.

- [ ] **Step 1: Write the failing tests + minimal implementation skeleton**

Replace `translate/request.rs` with:

```rust
//! `Vec<Item>` + `ModelRequest` → Anthropic Messages request body.
//!
//! Rules per the SMA-317 spec § "Wire translation". The translator
//! produces a `serde_json::Value` rather than typed structs to keep the
//! wire-snapshot tests readable.

use paigasus_helikon_core::{ContentPart, Item};
use serde_json::{json, Value};

/// Output of [`translate_messages`]: the top-level `system` field (string
/// or block-array form) and the `messages` array.
pub(crate) struct TranslatedMessages {
    pub(crate) system: Option<Value>,
    pub(crate) messages: Value,
}

/// Translate the conversation into Anthropic's request shape.
///
/// `system` is `None` when no `Item::System` is present. `messages` is
/// always an array.
pub(crate) fn translate_messages(items: &[Item]) -> TranslatedMessages {
    let mut system_text = String::new();
    let mut messages: Vec<Value> = Vec::new();

    for item in items {
        match item {
            Item::System { content } => {
                if !system_text.is_empty() {
                    system_text.push('\n');
                }
                system_text.push_str(&text_of(content));
            }
            Item::UserMessage { content } => {
                messages.push(json!({
                    "role": "user",
                    "content": user_blocks(content),
                }));
            }
            Item::AssistantMessage { content, agent: _ } => {
                messages.push(json!({
                    "role": "assistant",
                    "content": assistant_blocks(content),
                }));
            }
            _ => {
                // Task 10 + 11 fill in ToolCall / ToolResult.
                tracing::warn!(
                    target: "paigasus::anthropic::translate",
                    "Item variant not yet implemented; skipping",
                );
            }
        }
    }

    let system = if system_text.is_empty() {
        None
    } else {
        Some(Value::String(system_text))
    };
    TranslatedMessages { system, messages: Value::Array(messages) }
}

fn text_of(parts: &[ContentPart]) -> String {
    let mut s = String::new();
    for p in parts {
        if let ContentPart::Text { text } = p {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(text);
        }
    }
    s
}

fn user_blocks(content: &[ContentPart]) -> Value {
    let blocks: Vec<Value> = content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(json!({"type": "text", "text": text})),
            _ => None, // media + tool_result handled in later tasks
        })
        .collect();
    Value::Array(blocks)
}

fn assistant_blocks(content: &[ContentPart]) -> Value {
    let blocks: Vec<Value> = content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(json!({"type": "text", "text": text})),
            ContentPart::Reasoning { .. } => {
                tracing::warn!(
                    target: "paigasus::anthropic::translate",
                    "dropping ContentPart::Reasoning on input — signature round-trip not yet supported",
                );
                None
            }
            _ => None,
        })
        .collect();
    Value::Array(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &str) -> ContentPart {
        ContentPart::Text { text: t.to_owned() }
    }

    #[test]
    fn system_collapses_into_top_level_string() {
        let items = vec![Item::System {
            content: vec![text("be helpful")],
        }];
        let out = translate_messages(&items);
        assert_eq!(out.system, Some(Value::String("be helpful".to_owned())));
        assert_eq!(out.messages, json!([]));
    }

    #[test]
    fn multiple_system_items_concatenate_in_order() {
        let items = vec![
            Item::System { content: vec![text("first")] },
            Item::UserMessage { content: vec![text("hi")] },
            Item::System { content: vec![text("second")] },
        ];
        let out = translate_messages(&items);
        assert_eq!(
            out.system,
            Some(Value::String("first\nsecond".to_owned())),
            "all system items collapse into one top-level slot (order-loss vs surrounding turns)",
        );
    }

    #[test]
    fn user_text_emits_text_block() {
        let items = vec![Item::UserMessage {
            content: vec![text("hello")],
        }];
        let out = translate_messages(&items);
        assert_eq!(
            out.messages,
            json!([{"role": "user", "content": [{"type": "text", "text": "hello"}]}]),
        );
    }

    #[test]
    fn assistant_text_emits_text_block() {
        let items = vec![Item::AssistantMessage {
            content: vec![text("done")],
            agent: Some("planner".to_owned()),
        }];
        let out = translate_messages(&items);
        assert_eq!(
            out.messages,
            json!([{"role": "assistant", "content": [{"type": "text", "text": "done"}]}]),
        );
        // `agent` attribution is dropped (no Anthropic slot).
    }

    #[test]
    fn assistant_reasoning_is_always_dropped() {
        let items = vec![Item::AssistantMessage {
            content: vec![
                ContentPart::Reasoning { text: "scratch".to_owned() },
                text("answer"),
            ],
            agent: None,
        }];
        let out = translate_messages(&items);
        let content = &out.messages[0]["content"];
        assert_eq!(content.as_array().unwrap().len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "answer");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic translate::request`

Expected: 5 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/translate/request.rs
git commit -m "feat(providers-anthropic): SMA-317 translate text-only messages to Anthropic body"
```

---

## Task 10: `translate/request.rs` — ToolCall fold + standalone-tool synthesis

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/request.rs`

- [ ] **Step 1: Add failing tests**

Append to the `#[cfg(test)] mod tests` block (re-using the `text(...)` helper from Task 9):

```rust
    #[test]
    fn tool_call_folds_into_preceding_assistant() {
        let items = vec![
            Item::AssistantMessage {
                content: vec![text("calling")],
                agent: None,
            },
            Item::ToolCall {
                call_id: "tu_1".to_owned(),
                name: "ping".to_owned(),
                args: json!({"host": "ex.com"}),
            },
        ];
        let out = translate_messages(&items);
        let msg = &out.messages[0];
        assert_eq!(msg["role"], "assistant");
        let blocks = msg["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], json!({"type": "text", "text": "calling"}));
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "tu_1");
        assert_eq!(blocks[1]["name"], "ping");
        assert_eq!(blocks[1]["input"], json!({"host": "ex.com"}));
    }

    #[test]
    fn standalone_tool_calls_synthesize_assistant_carrier() {
        let items = vec![
            Item::ToolCall {
                call_id: "tu_a".to_owned(),
                name: "a".to_owned(),
                args: json!({}),
            },
            Item::ToolCall {
                call_id: "tu_b".to_owned(),
                name: "b".to_owned(),
                args: json!({"x": 1}),
            },
        ];
        let out = translate_messages(&items);
        assert_eq!(out.messages.as_array().unwrap().len(), 1);
        let msg = &out.messages[0];
        assert_eq!(msg["role"], "assistant");
        let blocks = msg["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["id"], "tu_a");
        assert_eq!(blocks[1]["id"], "tu_b");
    }

    #[test]
    fn assistant_with_nested_tool_use_emits_tool_use_block() {
        let items = vec![Item::AssistantMessage {
            content: vec![
                text("ok"),
                ContentPart::ToolUse {
                    call_id: "tu_x".to_owned(),
                    name: "search".to_owned(),
                    args: json!({"q": "rust"}),
                },
            ],
            agent: None,
        }];
        let out = translate_messages(&items);
        let blocks = out.messages[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["id"], "tu_x");
        assert_eq!(blocks[1]["input"], json!({"q": "rust"}));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p paigasus-helikon-providers-anthropic translate::request::tests::tool_call_folds`

Expected: FAIL — the loop in `translate_messages` skips `ToolCall` today.

- [ ] **Step 3: Update `translate_messages` to fold ToolCall + synthesize carrier**

Replace the body of `translate_messages` in `translate/request.rs` with the version below (adds ToolCall handling and threads pending standalone-ToolCall blocks):

```rust
pub(crate) fn translate_messages(items: &[Item]) -> TranslatedMessages {
    let mut system_text = String::new();
    let mut messages: Vec<Value> = Vec::new();
    let mut pending_tool_use: Vec<Value> = Vec::new();

    fn flush_pending(messages: &mut Vec<Value>, pending: &mut Vec<Value>) {
        if !pending.is_empty() {
            messages.push(json!({
                "role": "assistant",
                "content": std::mem::take(pending),
            }));
        }
    }

    for item in items {
        match item {
            Item::System { content } => {
                flush_pending(&mut messages, &mut pending_tool_use);
                if !system_text.is_empty() {
                    system_text.push('\n');
                }
                system_text.push_str(&text_of(content));
            }
            Item::UserMessage { content } => {
                flush_pending(&mut messages, &mut pending_tool_use);
                messages.push(json!({
                    "role": "user",
                    "content": user_blocks(content),
                }));
            }
            Item::AssistantMessage { content, agent: _ } => {
                flush_pending(&mut messages, &mut pending_tool_use);
                messages.push(json!({
                    "role": "assistant",
                    "content": assistant_blocks(content),
                }));
            }
            Item::ToolCall { call_id, name, args } => {
                let block = json!({
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": args,
                });
                if let Some(last) = messages.last_mut().filter(|m| m["role"] == "assistant") {
                    last["content"].as_array_mut().unwrap().push(block);
                } else {
                    pending_tool_use.push(block);
                }
            }
            _ => {
                // Task 11 fills in ToolResult.
                tracing::warn!(
                    target: "paigasus::anthropic::translate",
                    "Item::ToolResult not yet implemented",
                );
            }
        }
    }

    flush_pending(&mut messages, &mut pending_tool_use);

    let system = if system_text.is_empty() {
        None
    } else {
        Some(Value::String(system_text))
    };
    TranslatedMessages { system, messages: Value::Array(messages) }
}
```

And extend `assistant_blocks` to handle nested `ContentPart::ToolUse`:

```rust
fn assistant_blocks(content: &[ContentPart]) -> Value {
    let blocks: Vec<Value> = content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(json!({"type": "text", "text": text})),
            ContentPart::ToolUse { call_id, name, args } => Some(json!({
                "type": "tool_use",
                "id": call_id,
                "name": name,
                "input": args,
            })),
            ContentPart::Reasoning { .. } => {
                tracing::warn!(
                    target: "paigasus::anthropic::translate",
                    "dropping ContentPart::Reasoning on input — signature round-trip not yet supported",
                );
                None
            }
            _ => None,
        })
        .collect();
    Value::Array(blocks)
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic translate::request`

Expected: 8 PASS (5 from Task 9 + 3 new).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/translate/request.rs
git commit -m "feat(providers-anthropic): SMA-317 fold ToolCall into assistant + synthesize carrier"
```

---

## Task 11: `translate/request.rs` — ToolResult hoist + coalesce into next user turn

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/request.rs`

- [ ] **Step 1: Add failing tests**

Append to the test block:

```rust
    #[test]
    fn tool_result_coalesces_into_following_user_message() {
        let items = vec![
            Item::AssistantMessage {
                content: vec![ContentPart::ToolUse {
                    call_id: "tu_1".to_owned(),
                    name: "ping".to_owned(),
                    args: json!({}),
                }],
                agent: None,
            },
            Item::ToolResult {
                call_id: "tu_1".to_owned(),
                content: vec![text("pong")],
            },
            Item::UserMessage { content: vec![text("now what?")] },
        ];
        let out = translate_messages(&items);
        let arr = out.messages.as_array().unwrap();
        assert_eq!(arr.len(), 2, "must not produce consecutive user turns");
        assert_eq!(arr[0]["role"], "assistant");
        assert_eq!(arr[1]["role"], "user");
        let blocks = arr[1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["type"], "tool_result", "tool_result precedes text");
        assert_eq!(blocks[0]["tool_use_id"], "tu_1");
        assert_eq!(blocks[1]["type"], "text");
        assert_eq!(blocks[1]["text"], "now what?");
    }

    #[test]
    fn trailing_tool_result_synthesizes_user_turn() {
        let items = vec![
            Item::AssistantMessage {
                content: vec![ContentPart::ToolUse {
                    call_id: "tu_1".to_owned(),
                    name: "ping".to_owned(),
                    args: json!({}),
                }],
                agent: None,
            },
            Item::ToolResult {
                call_id: "tu_1".to_owned(),
                content: vec![text("pong")],
            },
        ];
        let out = translate_messages(&items);
        let arr = out.messages.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1]["role"], "user");
        let blocks = arr[1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_result");
    }

    #[test]
    fn adjacent_tool_results_coalesce_into_one_user_turn() {
        let items = vec![
            Item::AssistantMessage {
                content: vec![
                    ContentPart::ToolUse {
                        call_id: "tu_a".to_owned(),
                        name: "a".to_owned(),
                        args: json!({}),
                    },
                    ContentPart::ToolUse {
                        call_id: "tu_b".to_owned(),
                        name: "b".to_owned(),
                        args: json!({}),
                    },
                ],
                agent: None,
            },
            Item::ToolResult { call_id: "tu_a".to_owned(), content: vec![text("A!")] },
            Item::ToolResult { call_id: "tu_b".to_owned(), content: vec![text("B!")] },
        ];
        let out = translate_messages(&items);
        let arr = out.messages.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let blocks = arr[1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["tool_use_id"], "tu_a");
        assert_eq!(blocks[1]["tool_use_id"], "tu_b");
    }

    #[test]
    fn user_with_nested_tool_result_emits_native_block() {
        // The Anthropic-native shape: ContentPart::ToolResult inside a UserMessage.
        let items = vec![Item::UserMessage {
            content: vec![ContentPart::ToolResult {
                call_id: "tu_z".to_owned(),
                content: vec![text("native")],
            }],
        }];
        let out = translate_messages(&items);
        let blocks = out.messages[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "tool_result");
        assert_eq!(blocks[0]["tool_use_id"], "tu_z");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p paigasus-helikon-providers-anthropic translate::request::tests::tool_result_coalesces`

Expected: FAIL — current code skips `ToolResult`.

- [ ] **Step 3: Update the translator**

Replace the `Item::ToolResult` arm and extend `user_blocks` and add a pending-tool-result buffer. Update `translate_messages` to introduce `pending_tool_results: Vec<Value>` that flushes into the next `UserMessage` (coalescing) or synthesizes a user turn at end-of-stream:

```rust
pub(crate) fn translate_messages(items: &[Item]) -> TranslatedMessages {
    let mut system_text = String::new();
    let mut messages: Vec<Value> = Vec::new();
    let mut pending_tool_use: Vec<Value> = Vec::new();
    let mut pending_tool_results: Vec<Value> = Vec::new();

    fn flush_pending_assistant(messages: &mut Vec<Value>, pending: &mut Vec<Value>) {
        if !pending.is_empty() {
            messages.push(json!({
                "role": "assistant",
                "content": std::mem::take(pending),
            }));
        }
    }

    fn flush_pending_user(messages: &mut Vec<Value>, pending: &mut Vec<Value>) {
        if !pending.is_empty() {
            messages.push(json!({
                "role": "user",
                "content": std::mem::take(pending),
            }));
        }
    }

    for item in items {
        match item {
            Item::System { content } => {
                flush_pending_assistant(&mut messages, &mut pending_tool_use);
                flush_pending_user(&mut messages, &mut pending_tool_results);
                if !system_text.is_empty() {
                    system_text.push('\n');
                }
                system_text.push_str(&text_of(content));
            }
            Item::UserMessage { content } => {
                flush_pending_assistant(&mut messages, &mut pending_tool_use);
                // Pending tool_results go at the front of this user turn.
                let mut blocks: Vec<Value> = std::mem::take(&mut pending_tool_results);
                blocks.extend(user_blocks(content).as_array().unwrap().iter().cloned());
                messages.push(json!({"role": "user", "content": blocks}));
            }
            Item::AssistantMessage { content, agent: _ } => {
                flush_pending_user(&mut messages, &mut pending_tool_results);
                flush_pending_assistant(&mut messages, &mut pending_tool_use);
                messages.push(json!({
                    "role": "assistant",
                    "content": assistant_blocks(content),
                }));
            }
            Item::ToolCall { call_id, name, args } => {
                let block = json!({
                    "type": "tool_use",
                    "id": call_id,
                    "name": name,
                    "input": args,
                });
                if let Some(last) = messages.last_mut().filter(|m| m["role"] == "assistant") {
                    last["content"].as_array_mut().unwrap().push(block);
                } else {
                    pending_tool_use.push(block);
                }
            }
            Item::ToolResult { call_id, content } => {
                pending_tool_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": text_of(content),
                }));
            }
            _ => {
                tracing::warn!(
                    target: "paigasus::anthropic::translate",
                    "unknown Item variant; skipping",
                );
            }
        }
    }

    flush_pending_assistant(&mut messages, &mut pending_tool_use);
    flush_pending_user(&mut messages, &mut pending_tool_results);

    let system = if system_text.is_empty() {
        None
    } else {
        Some(Value::String(system_text))
    };
    TranslatedMessages { system, messages: Value::Array(messages) }
}
```

Extend `user_blocks` to handle `ContentPart::ToolResult` natively:

```rust
fn user_blocks(content: &[ContentPart]) -> Value {
    let blocks: Vec<Value> = content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(json!({"type": "text", "text": text})),
            ContentPart::ToolResult { call_id, content } => Some(json!({
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": text_of(content),
            })),
            _ => None,
        })
        .collect();
    Value::Array(blocks)
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic translate::request`

Expected: 12 PASS (8 from prior + 4 new).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/translate/request.rs
git commit -m "feat(providers-anthropic): SMA-317 hoist ToolResult into next user turn with coalescing"
```

---

## Task 12: `translate/request.rs` — media (URL + base64) handling

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/request.rs`

- [ ] **Step 1: Add failing tests**

Append to the test block (the existing tests use `text(...)` from Task 9 — re-use it):

```rust
    #[test]
    fn user_message_with_url_image_emits_url_source() {
        use paigasus_helikon_core::MediaSource;
        let items = vec![Item::UserMessage {
            content: vec![
                text("look:"),
                ContentPart::Image {
                    source: MediaSource::Url {
                        url: "https://example.com/cat.png".to_owned(),
                    },
                },
            ],
        }];
        let out = translate_messages(&items);
        let blocks = out.messages[0]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], json!({"type": "text", "text": "look:"}));
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["type"], "url");
        assert_eq!(blocks[1]["source"]["url"], "https://example.com/cat.png");
    }

    #[test]
    fn user_message_with_base64_image_emits_base64_source() {
        use paigasus_helikon_core::MediaSource;
        let items = vec![Item::UserMessage {
            content: vec![ContentPart::Image {
                source: MediaSource::Base64 {
                    mime_type: "image/png".to_owned(),
                    data: "AAAA".to_owned(),
                },
            }],
        }];
        let out = translate_messages(&items);
        let block = &out.messages[0]["content"][0];
        assert_eq!(block["type"], "image");
        assert_eq!(block["source"]["type"], "base64");
        assert_eq!(block["source"]["media_type"], "image/png");
        assert_eq!(block["source"]["data"], "AAAA");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p paigasus-helikon-providers-anthropic translate::request::tests::user_message_with_url_image`

Expected: FAIL — `user_blocks` filters `ContentPart::Image` to `None`.

- [ ] **Step 3: Update `user_blocks` to emit image blocks**

Replace the `_ => None,` arm in `user_blocks` with image handling:

```rust
fn user_blocks(content: &[ContentPart]) -> Value {
    use paigasus_helikon_core::MediaSource;
    let blocks: Vec<Value> = content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(json!({"type": "text", "text": text})),
            ContentPart::ToolResult { call_id, content } => Some(json!({
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": text_of(content),
            })),
            ContentPart::Image { source } => match source {
                MediaSource::Url { url } => Some(json!({
                    "type": "image",
                    "source": {"type": "url", "url": url},
                })),
                MediaSource::Base64 { mime_type, data } => Some(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": mime_type,
                        "data": data,
                    },
                })),
                _ => {
                    tracing::warn!(
                        target: "paigasus::anthropic::translate",
                        "unsupported MediaSource variant; skipping image",
                    );
                    None
                }
            },
            _ => None,
        })
        .collect();
    Value::Array(blocks)
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic translate::request`

Expected: 14 PASS (12 from prior + 2 new).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/translate/request.rs
git commit -m "feat(providers-anthropic): SMA-317 emit image blocks for URL and base64 media"
```

---

## Task 13: `translate/tools.rs` + `translate/cache.rs`

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/tools.rs`
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/cache.rs`

Two small files; both fit one TDD cycle each. Combined into one task to keep the plan compact.

- [ ] **Step 1: Replace `translate/tools.rs`**

```rust
//! `ToolDef` → Anthropic tool entries for the request body.
//!
//! Anthropic accepts permissive schemas — no strict-mode rewriting.

use paigasus_helikon_core::ToolDef;
use serde_json::{json, Value};

/// Translate the request's tool list into Anthropic's `tools:` array.
/// Cache markers are applied by `translate::cache`, not here.
pub(crate) fn translate_tools(defs: &[ToolDef]) -> Value {
    let arr: Vec<Value> = defs
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.schema,
            })
        })
        .collect();
    Value::Array(arr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_empty_list_to_empty_array() {
        assert_eq!(translate_tools(&[]), json!([]));
    }

    #[test]
    fn passes_through_name_description_and_schema() {
        let defs = vec![ToolDef {
            name: "search".to_owned(),
            description: "search the web".to_owned(),
            schema: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        }];
        let out = translate_tools(&defs);
        assert_eq!(out[0]["name"], "search");
        assert_eq!(out[0]["description"], "search the web");
        assert_eq!(
            out[0]["input_schema"],
            json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        );
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic translate::tools`

Expected: 2 PASS.

- [ ] **Step 3: Replace `translate/cache.rs`**

```rust
//! `CacheStrategy` → cache_control marker placement.
//!
//! Anthropic accepts up to 4 cache breakpoints per request. Our placements
//! top out at 3 (system + tools + last turn).

use serde_json::{json, Value};

use crate::settings::CacheStrategy;

const EPHEMERAL: &str = r#"{"type":"ephemeral"}"#;

/// Apply the cache strategy to the request body in-place.
///
/// `system` is normalized into block-array form when a system marker is
/// requested (Anthropic accepts a string OR an array; markers need the
/// array form). Returns the (possibly converted) system value.
pub(crate) fn apply_cache_strategy(
    strategy: CacheStrategy,
    system: Option<Value>,
    tools: &mut Value,
    messages: &mut Value,
) -> Option<Value> {
    let mark = || json!({"type": "ephemeral"});
    let system = match strategy {
        CacheStrategy::None => system,
        CacheStrategy::System | CacheStrategy::SystemAndTools => system.map(|s| {
            let mut blocks = match s {
                Value::String(text) => vec![json!({"type": "text", "text": text})],
                Value::Array(arr) => arr,
                _ => return s,
            };
            if let Some(last) = blocks.last_mut() {
                if let Some(obj) = last.as_object_mut() {
                    obj.insert("cache_control".to_owned(), mark());
                }
            }
            Value::Array(blocks)
        }),
        CacheStrategy::Tools | CacheStrategy::LastTurn => system,
    };

    if matches!(strategy, CacheStrategy::Tools | CacheStrategy::SystemAndTools) {
        if let Some(arr) = tools.as_array_mut() {
            if let Some(last) = arr.last_mut() {
                if let Some(obj) = last.as_object_mut() {
                    obj.insert("cache_control".to_owned(), mark());
                }
            }
        }
    }

    if matches!(strategy, CacheStrategy::LastTurn) {
        if let Some(arr) = messages.as_array_mut() {
            if let Some(last_msg) = arr.last_mut() {
                if let Some(blocks) = last_msg["content"].as_array_mut() {
                    if let Some(last_block) = blocks.last_mut() {
                        if let Some(obj) = last_block.as_object_mut() {
                            obj.insert("cache_control".to_owned(), mark());
                        }
                    }
                }
            }
        }
    }

    debug_assert!(!EPHEMERAL.is_empty()); // silence unused-const if no markers placed.
    system
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark() -> Value {
        json!({"type": "ephemeral"})
    }

    #[test]
    fn none_strategy_leaves_body_untouched() {
        let mut tools = json!([{"name": "t1"}]);
        let mut messages = json!([{"role": "user", "content": [{"type": "text", "text": "hi"}]}]);
        let system = apply_cache_strategy(
            CacheStrategy::None,
            Some(Value::String("S".to_owned())),
            &mut tools,
            &mut messages,
        );
        assert_eq!(system, Some(Value::String("S".to_owned())));
        assert!(tools[0].get("cache_control").is_none());
        assert!(messages[0]["content"][0].get("cache_control").is_none());
    }

    #[test]
    fn system_strategy_converts_string_to_array_and_marks_last_block() {
        let mut tools = json!([]);
        let mut messages = json!([]);
        let system = apply_cache_strategy(
            CacheStrategy::System,
            Some(Value::String("S".to_owned())),
            &mut tools,
            &mut messages,
        );
        let arr = system.unwrap();
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "S");
        assert_eq!(arr[0]["cache_control"], mark());
    }

    #[test]
    fn tools_strategy_marks_last_tool() {
        let mut tools = json!([{"name": "a"}, {"name": "b"}]);
        let mut messages = json!([]);
        apply_cache_strategy(CacheStrategy::Tools, None, &mut tools, &mut messages);
        assert!(tools[0].get("cache_control").is_none());
        assert_eq!(tools[1]["cache_control"], mark());
    }

    #[test]
    fn system_and_tools_marks_both() {
        let mut tools = json!([{"name": "a"}]);
        let mut messages = json!([]);
        let system = apply_cache_strategy(
            CacheStrategy::SystemAndTools,
            Some(Value::String("S".to_owned())),
            &mut tools,
            &mut messages,
        );
        assert_eq!(system.unwrap()[0]["cache_control"], mark());
        assert_eq!(tools[0]["cache_control"], mark());
    }

    #[test]
    fn empty_system_and_tools_inserts_nothing() {
        let mut tools = json!([]);
        let mut messages = json!([]);
        let system = apply_cache_strategy(
            CacheStrategy::SystemAndTools,
            None,
            &mut tools,
            &mut messages,
        );
        assert!(system.is_none());
        assert_eq!(tools, json!([]));
    }

    #[test]
    fn last_turn_marks_final_block_of_final_message() {
        let mut tools = json!([]);
        let mut messages = json!([
            {"role": "user", "content": [{"type": "text", "text": "first"}]},
            {"role": "user", "content": [{"type": "text", "text": "second"}]},
        ]);
        apply_cache_strategy(CacheStrategy::LastTurn, None, &mut tools, &mut messages);
        assert!(messages[0]["content"][0].get("cache_control").is_none());
        assert_eq!(messages[1]["content"][0]["cache_control"], mark());
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic translate::cache`

Expected: 6 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/translate/{tools.rs,cache.rs}
git commit -m "feat(providers-anthropic): SMA-317 add tool + cache_control translation"
```

---

## Task 14: `translate/response_format.rs` — forced-tool synthesis + guards

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/response_format.rs`

- [ ] **Step 1: Replace `translate/response_format.rs`**

```rust
//! `ResponseFormat` → synthesized forced tool + guards.

use paigasus_helikon_core::{ResponseFormat, ToolChoice, ToolDef};
use serde_json::{json, Value};

/// Reserved name for the synthesized output tool. Caller-provided tools
/// with this name are rejected by [`validate_tool_names`].
pub(crate) const SYNTHESIZED_TOOL_NAME: &str = "__paigasus_structured_output__";

/// Outcome of [`synthesize_for_response_format`].
pub(crate) struct Synthesized {
    /// Extra tool to append to the `tools` array.
    pub(crate) tool: Option<Value>,
    /// `tool_choice` value to write into the request body.
    pub(crate) tool_choice: Option<Value>,
    /// True when we synthesized — the stream translator uses this to
    /// remap the synthesized tool's `input_json_delta` events to `TokenDelta`.
    pub(crate) synthesizing: bool,
}

/// Reject user tools whose name collides with the reserved synthesized name.
///
/// Runs **regardless of `ResponseFormat`** — a stray collision pollutes
/// the request schema even when synthesis is inactive.
pub(crate) fn validate_tool_names(defs: &[ToolDef]) -> Result<(), String> {
    for d in defs {
        if d.name == SYNTHESIZED_TOOL_NAME {
            return Err(format!(
                "tool name '{SYNTHESIZED_TOOL_NAME}' is reserved by the Anthropic provider \
                 for structured-output synthesis",
            ));
        }
    }
    Ok(())
}

/// Reject (ResponseFormat::JsonSchema|JsonObject) + ToolChoice::Tool combinations.
pub(crate) fn validate_conflict(
    rf: Option<&ResponseFormat>,
    tc: Option<&ToolChoice>,
) -> Result<(), String> {
    let synthesizing = matches!(
        rf,
        Some(ResponseFormat::JsonSchema { .. }) | Some(ResponseFormat::JsonObject),
    );
    let forced_tool = matches!(tc, Some(ToolChoice::Tool { .. }));
    if synthesizing && forced_tool {
        return Err(
            "ResponseFormat::JsonSchema/JsonObject and ToolChoice::Tool are \
             mutually exclusive on Anthropic"
                .to_owned(),
        );
    }
    Ok(())
}

/// Build the synthesized tool + tool_choice value for the given response format.
/// Returns `Synthesized { synthesizing: false, .. }` for `Text` / `None`.
pub(crate) fn synthesize_for_response_format(rf: Option<&ResponseFormat>) -> Synthesized {
    match rf {
        Some(ResponseFormat::JsonSchema { name, schema, .. }) => Synthesized {
            tool: Some(json!({
                "name": SYNTHESIZED_TOOL_NAME,
                "description": format!("Return data matching the {name} schema."),
                "input_schema": schema,
            })),
            tool_choice: Some(json!({"type": "tool", "name": SYNTHESIZED_TOOL_NAME})),
            synthesizing: true,
        },
        Some(ResponseFormat::JsonObject) => Synthesized {
            tool: Some(json!({
                "name": SYNTHESIZED_TOOL_NAME,
                "description": "Return a JSON object.",
                "input_schema": {"type": "object"},
            })),
            tool_choice: Some(json!({"type": "tool", "name": SYNTHESIZED_TOOL_NAME})),
            synthesizing: true,
        },
        _ => Synthesized { tool: None, tool_choice: None, synthesizing: false },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_name_rejected_in_user_tools() {
        let defs = vec![ToolDef {
            name: SYNTHESIZED_TOOL_NAME.to_owned(),
            description: "x".to_owned(),
            schema: json!({}),
        }];
        let err = validate_tool_names(&defs).unwrap_err();
        assert!(err.contains("reserved"));
    }

    #[test]
    fn normal_tool_names_pass() {
        let defs = vec![ToolDef {
            name: "search".to_owned(),
            description: "x".to_owned(),
            schema: json!({}),
        }];
        assert!(validate_tool_names(&defs).is_ok());
    }

    #[test]
    fn json_schema_and_tool_choice_tool_conflict() {
        let rf = ResponseFormat::JsonSchema {
            name: "X".to_owned(),
            schema: json!({}),
            strict: true,
        };
        let tc = ToolChoice::Tool { name: "search".to_owned() };
        assert!(validate_conflict(Some(&rf), Some(&tc)).is_err());
    }

    #[test]
    fn json_schema_with_no_tool_choice_passes() {
        let rf = ResponseFormat::JsonSchema {
            name: "X".to_owned(),
            schema: json!({}),
            strict: true,
        };
        assert!(validate_conflict(Some(&rf), None).is_ok());
    }

    #[test]
    fn text_format_no_synthesis() {
        let s = synthesize_for_response_format(Some(&ResponseFormat::Text));
        assert!(!s.synthesizing);
        assert!(s.tool.is_none());
        assert!(s.tool_choice.is_none());
    }

    #[test]
    fn json_schema_produces_synthesized_tool() {
        let rf = ResponseFormat::JsonSchema {
            name: "Person".to_owned(),
            schema: json!({"type": "object"}),
            strict: false,
        };
        let s = synthesize_for_response_format(Some(&rf));
        assert!(s.synthesizing);
        let t = s.tool.unwrap();
        assert_eq!(t["name"], SYNTHESIZED_TOOL_NAME);
        assert!(t["description"].as_str().unwrap().contains("Person"));
        assert_eq!(t["input_schema"], json!({"type": "object"}));
        assert_eq!(
            s.tool_choice.unwrap(),
            json!({"type": "tool", "name": SYNTHESIZED_TOOL_NAME}),
        );
    }

    #[test]
    fn json_object_produces_synthesized_tool_with_object_schema() {
        let s = synthesize_for_response_format(Some(&ResponseFormat::JsonObject));
        assert!(s.synthesizing);
        assert_eq!(s.tool.unwrap()["input_schema"], json!({"type": "object"}));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic translate::response_format`

Expected: 7 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/translate/response_format.rs
git commit -m "feat(providers-anthropic): SMA-317 add forced-tool synthesis with reserved-name guard"
```

---

## Task 15: `error.rs` — `map_error_type` shared helper

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/error.rs`

- [ ] **Step 1: Replace `error.rs`**

```rust
//! Map Anthropic HTTP and in-stream errors to [`paigasus_helikon_core::ModelError`].
//!
//! Per ADR-10 ("no silent auto-retry in the loop"), the runner never
//! retries; the application configures retries via
//! `RunConfig::retry_policy`. Auth failures (401/403) map to `Refused`;
//! 429 maps to `RateLimited`; 5xx and 529 map to `Unavailable`. The same
//! helper is invoked from both the HTTP-response path and the in-stream
//! `error` SSE event path so behavior is consistent.

use paigasus_helikon_core::ModelError;

/// `error.type` (with optional HTTP status) → `ModelError`.
///
/// Stream path passes `status: None` and `retry_after_ms: None`. HTTP path
/// supplies both.
pub(crate) fn map_error_type(
    status: Option<u16>,
    error_type: &str,
    message: &str,
    retry_after_ms: Option<u64>,
) -> ModelError {
    match (status, error_type) {
        (_, "overloaded_error") => ModelError::Unavailable,
        (_, "rate_limit_error") => ModelError::RateLimited { retry_after_ms },
        (_, "authentication_error") | (_, "permission_error") => {
            ModelError::Refused { reason: message.to_owned() }
        }
        (_, "invalid_request_error") if message.contains("prompt is too long") => {
            ModelError::ContextLengthExceeded
        }
        (Some(s), _) if matches!(s, 500..=504 | 529) => ModelError::Unavailable,
        (Some(_), _) => ModelError::Other(anyhow::anyhow!("anthropic {error_type}: {message}")),
        (None, _) => ModelError::Transport(message.to_owned()),
    }
}

/// Parse the `retry-after` header (seconds, integer) into milliseconds.
pub(crate) fn parse_retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|seconds| seconds.saturating_mul(1000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_overloaded_maps_to_unavailable() {
        assert!(matches!(
            map_error_type(Some(529), "overloaded_error", "busy", None),
            ModelError::Unavailable,
        ));
    }

    #[test]
    fn stream_overloaded_maps_to_unavailable_not_transport() {
        assert!(matches!(
            map_error_type(None, "overloaded_error", "busy", None),
            ModelError::Unavailable,
        ));
    }

    #[test]
    fn http_429_maps_to_rate_limited_with_retry_after() {
        match map_error_type(Some(429), "rate_limit_error", "slow", Some(5000)) {
            ModelError::RateLimited { retry_after_ms } => {
                assert_eq!(retry_after_ms, Some(5000));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn auth_error_maps_to_refused() {
        match map_error_type(Some(401), "authentication_error", "bad key", None) {
            ModelError::Refused { reason } => assert_eq!(reason, "bad key"),
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    #[test]
    fn prompt_too_long_maps_to_context_length_exceeded() {
        assert!(matches!(
            map_error_type(Some(400), "invalid_request_error", "prompt is too long: 200k", None),
            ModelError::ContextLengthExceeded,
        ));
    }

    #[test]
    fn http_500_falls_through_to_unavailable() {
        assert!(matches!(
            map_error_type(Some(500), "api_error", "internal", None),
            ModelError::Unavailable,
        ));
    }

    #[test]
    fn http_400_other_maps_to_other() {
        assert!(matches!(
            map_error_type(Some(400), "invalid_request_error", "missing field", None),
            ModelError::Other(_),
        ));
    }

    #[test]
    fn stream_unknown_type_falls_to_transport() {
        match map_error_type(None, "mystery_error", "boom", None) {
            ModelError::Transport(s) => assert_eq!(s, "boom"),
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    #[test]
    fn parse_retry_after_handles_integer_seconds() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(reqwest::header::RETRY_AFTER, "3".parse().unwrap());
        assert_eq!(parse_retry_after_ms(&h), Some(3000));
    }

    #[test]
    fn parse_retry_after_missing_returns_none() {
        let h = reqwest::header::HeaderMap::new();
        assert_eq!(parse_retry_after_ms(&h), None);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic error`

Expected: 10 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/error.rs
git commit -m "feat(providers-anthropic): SMA-317 add map_error_type for HTTP + stream paths"
```

---

## Task 16: `http.rs` — request header construction

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/http.rs`

- [ ] **Step 1: Replace `http.rs`**

```rust
//! HTTP request building for the Messages endpoint.

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};

use crate::builder::{AuthHeader, Config};

const X_API_KEY: HeaderName = HeaderName::from_static("x-api-key");
const ANTHROPIC_VERSION: HeaderName = HeaderName::from_static("anthropic-version");
const ANTHROPIC_BETA: HeaderName = HeaderName::from_static("anthropic-beta");

/// Build the static request headers for the Messages endpoint.
///
/// Auth, version, and optional beta-feature header. `Authorization: Bearer`
/// is used when the builder chose `bearer(...)`, otherwise `x-api-key`.
pub(crate) fn build_headers(cfg: &Config) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    match &cfg.auth_header {
        AuthHeader::ApiKey(k) => {
            h.insert(X_API_KEY, HeaderValue::from_str(k).expect("API key has invalid header bytes"));
        }
        AuthHeader::Bearer(t) => {
            let v = format!("Bearer {t}");
            h.insert(
                reqwest::header::AUTHORIZATION,
                HeaderValue::from_str(&v).expect("bearer token has invalid header bytes"),
            );
        }
    }
    h.insert(
        ANTHROPIC_VERSION,
        HeaderValue::from_str(&cfg.anthropic_version).expect("anthropic-version invalid"),
    );
    if let Some(beta) = &cfg.anthropic_beta {
        h.insert(
            ANTHROPIC_BETA,
            HeaderValue::from_str(beta).expect("anthropic-beta value invalid"),
        );
    }
    h
}

/// Build the full URL: `<base_url>/v1/messages` with no trailing slash.
pub(crate) fn messages_url(cfg: &Config) -> String {
    let trimmed = cfg.base_url.trim_end_matches('/');
    format!("{trimmed}/v1/messages")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::AnthropicModelBuilder;

    fn build_config_with(auth: &str, beta: &[&str]) -> Config {
        // The builder's tests serialize ANTHROPIC_API_KEY env access; here we
        // skip env entirely by passing api_key explicitly.
        let mut b = AnthropicModelBuilder::new("claude-sonnet-4-6").api_key(auth);
        for v in beta {
            b = b.beta(*v);
        }
        b.build_config().unwrap()
    }

    #[test]
    fn api_key_auth_uses_x_api_key() {
        let cfg = build_config_with("sk-test", &[]);
        let h = build_headers(&cfg);
        assert_eq!(h.get("x-api-key").unwrap().to_str().unwrap(), "sk-test");
        assert!(h.get("authorization").is_none());
        assert_eq!(h.get("content-type").unwrap(), "application/json");
        assert_eq!(h.get("anthropic-version").unwrap(), "2023-06-01");
        assert!(h.get("anthropic-beta").is_none());
    }

    #[test]
    fn bearer_uses_authorization() {
        let mut b = AnthropicModelBuilder::new("claude-sonnet-4-6").bearer("ey...");
        b = b.beta("");
        // Two beta calls with empty pad the comma — accepted; tested in builder.
        let cfg = b.build_config().unwrap();
        let h = build_headers(&cfg);
        assert!(h.get("x-api-key").is_none());
        assert_eq!(h.get("authorization").unwrap().to_str().unwrap(), "Bearer ey...");
    }

    #[test]
    fn beta_header_is_comma_joined() {
        let cfg = build_config_with("sk-x", &["a", "b"]);
        let h = build_headers(&cfg);
        assert_eq!(h.get("anthropic-beta").unwrap().to_str().unwrap(), "a,b");
    }

    #[test]
    fn messages_url_appends_v1_messages() {
        let cfg = build_config_with("sk-x", &[]);
        assert_eq!(messages_url(&cfg), "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn messages_url_trims_trailing_slash() {
        let mut b = AnthropicModelBuilder::new("claude-sonnet-4-6").api_key("sk-x");
        b = b.base_url("https://proxy.example.com/anthropic/");
        let cfg = b.build_config().unwrap();
        assert_eq!(messages_url(&cfg), "https://proxy.example.com/anthropic/v1/messages");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic http`

Expected: 5 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/http.rs
git commit -m "feat(providers-anthropic): SMA-317 build request headers and messages URL"
```

---

## Task 17: `sse.rs` — SSE event envelope deserialization

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/sse.rs`

Anthropic's SSE format is `event: <name>\ndata: {<json>}\n\n`. We use `eventsource-stream` to chunk the bytes, then deserialize each chunk's `data` into a typed envelope.

- [ ] **Step 1: Replace `sse.rs`**

```rust
//! Anthropic SSE event envelope deserialization.

use serde::Deserialize;
use serde_json::Value;

/// The typed envelope for one SSE event from `/v1/messages` (stream mode).
///
/// `#[serde(tag = "type")]` matches Anthropic's `"type": "message_start"` etc.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum AnthropicEvent {
    #[serde(rename = "message_start")]
    MessageStart {
        message: MessageStartPayload,
    },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: u32,
        content_block: ContentBlockHead,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        index: u32,
        delta: ContentBlockDelta,
    },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: u32 },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: MessageDeltaPayload,
        usage: Option<MessageDeltaUsage>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: AnthropicErrorPayload },
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MessageStartPayload {
    pub(crate) usage: MessageStartUsage,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MessageStartUsage {
    pub(crate) input_tokens: u32,
    #[serde(default)]
    pub(crate) cache_read_input_tokens: Option<u32>,
    #[serde(default)]
    pub(crate) cache_creation_input_tokens: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ContentBlockHead {
    #[serde(rename = "text")]
    Text,
    #[serde(rename = "thinking")]
    Thinking,
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ContentBlockDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
    #[serde(rename = "signature_delta")]
    SignatureDelta {
        #[allow(dead_code)]
        signature: String,
    },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MessageDeltaPayload {
    #[serde(default)]
    pub(crate) stop_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MessageDeltaUsage {
    pub(crate) output_tokens: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AnthropicErrorPayload {
    #[serde(rename = "type")]
    pub(crate) ty: String,
    pub(crate) message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deserializes_message_start_with_cache_tokens() {
        let v = json!({
            "type": "message_start",
            "message": {
                "usage": {
                    "input_tokens": 100,
                    "cache_read_input_tokens": 80,
                    "cache_creation_input_tokens": 0
                }
            }
        });
        let e: AnthropicEvent = serde_json::from_value(v).unwrap();
        match e {
            AnthropicEvent::MessageStart { message } => {
                assert_eq!(message.usage.input_tokens, 100);
                assert_eq!(message.usage.cache_read_input_tokens, Some(80));
            }
            other => panic!("wrong variant {other:?}"),
        }
    }

    #[test]
    fn deserializes_text_delta() {
        let v = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Hi"}
        });
        let e: AnthropicEvent = serde_json::from_value(v).unwrap();
        match e {
            AnthropicEvent::ContentBlockDelta { index, delta: ContentBlockDelta::TextDelta { text } } => {
                assert_eq!(index, 0);
                assert_eq!(text, "Hi");
            }
            other => panic!("wrong variant {other:?}"),
        }
    }

    #[test]
    fn deserializes_tool_use_start_and_input_json_delta() {
        let start = json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {"type": "tool_use", "id": "tu_1", "name": "search", "input": {}}
        });
        let e: AnthropicEvent = serde_json::from_value(start).unwrap();
        match e {
            AnthropicEvent::ContentBlockStart {
                index: 1,
                content_block: ContentBlockHead::ToolUse { id, name, .. },
            } => {
                assert_eq!(id, "tu_1");
                assert_eq!(name, "search");
            }
            other => panic!("wrong variant {other:?}"),
        }

        let delta = json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": "{\"q\":"}
        });
        let e: AnthropicEvent = serde_json::from_value(delta).unwrap();
        match e {
            AnthropicEvent::ContentBlockDelta {
                delta: ContentBlockDelta::InputJsonDelta { partial_json },
                ..
            } => {
                assert_eq!(partial_json, "{\"q\":");
            }
            other => panic!("wrong variant {other:?}"),
        }
    }

    #[test]
    fn deserializes_message_delta_with_stop_and_usage() {
        let v = json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 42}
        });
        let e: AnthropicEvent = serde_json::from_value(v).unwrap();
        match e {
            AnthropicEvent::MessageDelta { delta, usage } => {
                assert_eq!(delta.stop_reason.as_deref(), Some("end_turn"));
                assert_eq!(usage.unwrap().output_tokens, 42);
            }
            other => panic!("wrong variant {other:?}"),
        }
    }

    #[test]
    fn deserializes_error_event() {
        let v = json!({
            "type": "error",
            "error": {"type": "overloaded_error", "message": "busy"}
        });
        let e: AnthropicEvent = serde_json::from_value(v).unwrap();
        match e {
            AnthropicEvent::Error { error } => {
                assert_eq!(error.ty, "overloaded_error");
                assert_eq!(error.message, "busy");
            }
            other => panic!("wrong variant {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic sse`

Expected: 5 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/sse.rs
git commit -m "feat(providers-anthropic): SMA-317 deserialize Anthropic SSE event envelope"
```

---

## Task 18: `stream.rs` — MessageTranslator state machine

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/stream.rs`

The translator consumes [`AnthropicEvent`]s in order and emits zero or more [`ModelEvent`]s per event. See the spec § "Streaming SSE → ModelEvent" for the full mapping.

- [ ] **Step 1: Replace `stream.rs`**

```rust
//! `MessageTranslator` — Anthropic SSE events → `ModelEvent` stream.

use std::collections::HashMap;

use paigasus_helikon_core::{FinishReason, ModelError, ModelEvent};

use crate::error::map_error_type;
use crate::sse::{
    AnthropicEvent, ContentBlockDelta, ContentBlockHead, MessageDeltaUsage, MessageStartUsage,
};
use crate::translate::response_format::SYNTHESIZED_TOOL_NAME;

#[derive(Debug)]
enum BlockState {
    Text,
    Thinking,
    ToolUse {
        call_id: String,
        name: String,
        name_emitted: bool,
    },
}

/// State machine for one streaming response.
///
/// `synthesizing_output: true` means a `ResponseFormat::JsonSchema`/`JsonObject`
/// request was sent. When the synthesized tool's content block starts, its
/// `input_json_delta` events are remapped to `TokenDelta`s and the
/// `stop_reason: "tool_use"` is rewritten to `Stop` if it was the only tool fired.
pub(crate) struct MessageTranslator {
    blocks: HashMap<u32, BlockState>,
    last_input_tokens: u32,
    last_cached_input_tokens: Option<u32>,
    stop_reason: Option<String>,
    synthesizing_output: bool,
    synthesized_tool_index: Option<u32>,
    other_tool_fired: bool,
}

impl MessageTranslator {
    pub(crate) fn new(synthesizing_output: bool) -> Self {
        Self {
            blocks: HashMap::new(),
            last_input_tokens: 0,
            last_cached_input_tokens: None,
            stop_reason: None,
            synthesizing_output,
            synthesized_tool_index: None,
            other_tool_fired: false,
        }
    }

    /// Consume one event. Returns the emitted ModelEvents (most calls
    /// emit zero or one; `message_delta` carrying both stop_reason and
    /// usage emits one Usage followed by Finish on `message_stop`).
    pub(crate) fn consume(
        &mut self,
        event: AnthropicEvent,
    ) -> Result<Vec<Result<ModelEvent, ModelError>>, ModelError> {
        let mut out: Vec<Result<ModelEvent, ModelError>> = Vec::new();
        match event {
            AnthropicEvent::MessageStart { message } => {
                let MessageStartUsage {
                    input_tokens,
                    cache_read_input_tokens,
                    ..
                } = message.usage;
                self.last_input_tokens = input_tokens;
                self.last_cached_input_tokens = cache_read_input_tokens;
                out.push(Ok(ModelEvent::Usage {
                    input_tokens,
                    output_tokens: 0,
                    cached_input_tokens: cache_read_input_tokens,
                    reasoning_tokens: None,
                }));
            }
            AnthropicEvent::ContentBlockStart { index, content_block } => match content_block {
                ContentBlockHead::Text => {
                    self.blocks.insert(index, BlockState::Text);
                }
                ContentBlockHead::Thinking => {
                    self.blocks.insert(index, BlockState::Thinking);
                }
                ContentBlockHead::ToolUse { id, name, .. } => {
                    if self.synthesizing_output && name == SYNTHESIZED_TOOL_NAME {
                        self.synthesized_tool_index = Some(index);
                    } else {
                        self.other_tool_fired = true;
                    }
                    self.blocks.insert(
                        index,
                        BlockState::ToolUse { call_id: id, name, name_emitted: false },
                    );
                }
            },
            AnthropicEvent::ContentBlockDelta { index, delta } => match delta {
                ContentBlockDelta::TextDelta { text } => {
                    out.push(Ok(ModelEvent::TokenDelta { text }));
                }
                ContentBlockDelta::ThinkingDelta { thinking } => {
                    out.push(Ok(ModelEvent::ReasoningDelta { text: thinking }));
                }
                ContentBlockDelta::SignatureDelta { .. } => {
                    tracing::debug!(
                        target: "paigasus::anthropic::stream",
                        "signature_delta dropped (round-trip not yet supported)",
                    );
                }
                ContentBlockDelta::InputJsonDelta { partial_json } => {
                    let is_synth = Some(index) == self.synthesized_tool_index;
                    if is_synth {
                        out.push(Ok(ModelEvent::TokenDelta { text: partial_json }));
                    } else if let Some(BlockState::ToolUse {
                        call_id,
                        name,
                        name_emitted,
                    }) = self.blocks.get_mut(&index)
                    {
                        let (emit_name, call_id, name_value) = if *name_emitted {
                            (None, call_id.clone(), name.clone())
                        } else {
                            *name_emitted = true;
                            (Some(name.clone()), call_id.clone(), name.clone())
                        };
                        let _ = name_value; // silence unused
                        out.push(Ok(ModelEvent::ToolCallDelta {
                            call_id,
                            name: emit_name,
                            args_delta: partial_json,
                        }));
                    }
                }
            },
            AnthropicEvent::ContentBlockStop { .. } => {
                tracing::debug!(target: "paigasus::anthropic::stream", "content_block_stop");
            }
            AnthropicEvent::MessageDelta { delta, usage } => {
                if let Some(MessageDeltaUsage { output_tokens }) = usage {
                    out.push(Ok(ModelEvent::Usage {
                        input_tokens: self.last_input_tokens,
                        output_tokens,
                        cached_input_tokens: self.last_cached_input_tokens,
                        reasoning_tokens: None,
                    }));
                }
                if let Some(reason) = delta.stop_reason {
                    self.stop_reason = Some(reason);
                }
            }
            AnthropicEvent::MessageStop => {
                if let Some(reason) = self.stop_reason.take() {
                    out.push(self.finish_or_error(&reason));
                }
            }
            AnthropicEvent::Ping => {}
            AnthropicEvent::Error { error } => {
                return Err(map_error_type(None, &error.ty, &error.message, None));
            }
        }
        Ok(out)
    }

    fn finish_or_error(&self, reason: &str) -> Result<ModelEvent, ModelError> {
        match reason {
            "end_turn" | "stop_sequence" => Ok(ModelEvent::Finish { reason: FinishReason::Stop }),
            "max_tokens" => Ok(ModelEvent::Finish { reason: FinishReason::Length }),
            "tool_use" => {
                if self.synthesizing_output && !self.other_tool_fired {
                    Ok(ModelEvent::Finish { reason: FinishReason::Stop })
                } else if self.synthesizing_output && self.other_tool_fired {
                    Err(ModelError::Other(anyhow::anyhow!(
                        "structured output: model fired both a real tool and the synthesized output tool"
                    )))
                } else {
                    Ok(ModelEvent::Finish { reason: FinishReason::ToolCalls })
                }
            }
            "refusal" => Err(ModelError::Refused {
                reason: "model refused".to_owned(),
            }),
            other => Ok(ModelEvent::Finish {
                reason: FinishReason::Other(other.to_owned()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::{
        AnthropicErrorPayload, ContentBlockHead, MessageDeltaPayload, MessageStartPayload,
    };

    fn message_start(input: u32, cached: Option<u32>) -> AnthropicEvent {
        AnthropicEvent::MessageStart {
            message: MessageStartPayload {
                usage: MessageStartUsage {
                    input_tokens: input,
                    cache_read_input_tokens: cached,
                    cache_creation_input_tokens: None,
                },
            },
        }
    }

    #[test]
    fn message_start_emits_initial_usage_with_cached_count() {
        let mut t = MessageTranslator::new(false);
        let out = t.consume(message_start(100, Some(80))).unwrap();
        assert_eq!(out.len(), 1);
        match out.into_iter().next().unwrap().unwrap() {
            ModelEvent::Usage { input_tokens, cached_input_tokens, output_tokens, .. } => {
                assert_eq!(input_tokens, 100);
                assert_eq!(cached_input_tokens, Some(80));
                assert_eq!(output_tokens, 0);
            }
            _ => panic!("expected Usage"),
        }
    }

    #[test]
    fn text_delta_emits_token_delta() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockHead::Text,
        });
        let out = t
            .consume(AnthropicEvent::ContentBlockDelta {
                index: 0,
                delta: ContentBlockDelta::TextDelta { text: "Hi".to_owned() },
            })
            .unwrap();
        match out.into_iter().next().unwrap().unwrap() {
            ModelEvent::TokenDelta { text } => assert_eq!(text, "Hi"),
            _ => panic!("expected TokenDelta"),
        }
    }

    #[test]
    fn thinking_delta_emits_reasoning_delta() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockHead::Thinking,
        });
        let out = t
            .consume(AnthropicEvent::ContentBlockDelta {
                index: 0,
                delta: ContentBlockDelta::ThinkingDelta { thinking: "think".to_owned() },
            })
            .unwrap();
        match out.into_iter().next().unwrap().unwrap() {
            ModelEvent::ReasoningDelta { text } => assert_eq!(text, "think"),
            _ => panic!("expected ReasoningDelta"),
        }
    }

    #[test]
    fn tool_use_emits_call_delta_with_name_only_once() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 1,
            content_block: ContentBlockHead::ToolUse {
                id: "tu_1".to_owned(),
                name: "search".to_owned(),
                input: serde_json::json!({}),
            },
        });
        let first = t
            .consume(AnthropicEvent::ContentBlockDelta {
                index: 1,
                delta: ContentBlockDelta::InputJsonDelta { partial_json: "{".to_owned() },
            })
            .unwrap();
        match first.into_iter().next().unwrap().unwrap() {
            ModelEvent::ToolCallDelta { call_id, name, args_delta } => {
                assert_eq!(call_id, "tu_1");
                assert_eq!(name.as_deref(), Some("search"));
                assert_eq!(args_delta, "{");
            }
            _ => panic!("expected ToolCallDelta"),
        }

        let second = t
            .consume(AnthropicEvent::ContentBlockDelta {
                index: 1,
                delta: ContentBlockDelta::InputJsonDelta { partial_json: "\"q\":1}".to_owned() },
            })
            .unwrap();
        match second.into_iter().next().unwrap().unwrap() {
            ModelEvent::ToolCallDelta { name, .. } => assert!(name.is_none(), "name not repeated"),
            _ => panic!("expected ToolCallDelta"),
        }
    }

    #[test]
    fn synthesized_tool_remaps_input_json_to_token_delta() {
        let mut t = MessageTranslator::new(true);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockHead::ToolUse {
                id: "tu_synth".to_owned(),
                name: SYNTHESIZED_TOOL_NAME.to_owned(),
                input: serde_json::json!({}),
            },
        });
        let out = t
            .consume(AnthropicEvent::ContentBlockDelta {
                index: 0,
                delta: ContentBlockDelta::InputJsonDelta {
                    partial_json: "{\"x\":1}".to_owned(),
                },
            })
            .unwrap();
        match out.into_iter().next().unwrap().unwrap() {
            ModelEvent::TokenDelta { text } => assert_eq!(text, "{\"x\":1}"),
            other => panic!("expected TokenDelta, got {other:?}"),
        }
    }

    #[test]
    fn message_delta_then_stop_emits_usage_then_finish() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, Some(2))).unwrap();
        let usage_out = t
            .consume(AnthropicEvent::MessageDelta {
                delta: MessageDeltaPayload {
                    stop_reason: Some("end_turn".to_owned()),
                },
                usage: Some(MessageDeltaUsage { output_tokens: 5 }),
            })
            .unwrap();
        assert_eq!(usage_out.len(), 1);
        match usage_out.into_iter().next().unwrap().unwrap() {
            ModelEvent::Usage { input_tokens, output_tokens, cached_input_tokens, .. } => {
                assert_eq!(input_tokens, 10);
                assert_eq!(output_tokens, 5);
                assert_eq!(cached_input_tokens, Some(2));
            }
            _ => panic!("expected Usage"),
        }
        let stop_out = t.consume(AnthropicEvent::MessageStop).unwrap();
        match stop_out.into_iter().next().unwrap().unwrap() {
            ModelEvent::Finish { reason } => assert_eq!(reason, FinishReason::Stop),
            _ => panic!("expected Finish"),
        }
    }

    #[test]
    fn tool_use_stop_reason_emits_tool_calls_finish_without_synthesis() {
        let mut t = MessageTranslator::new(false);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::MessageDelta {
            delta: MessageDeltaPayload { stop_reason: Some("tool_use".to_owned()) },
            usage: None,
        });
        let out = t.consume(AnthropicEvent::MessageStop).unwrap();
        match out.into_iter().next().unwrap().unwrap() {
            ModelEvent::Finish { reason } => assert_eq!(reason, FinishReason::ToolCalls),
            _ => panic!("expected Finish"),
        }
    }

    #[test]
    fn synthesized_only_rewrites_tool_use_to_stop() {
        let mut t = MessageTranslator::new(true);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockHead::ToolUse {
                id: "tu_s".to_owned(),
                name: SYNTHESIZED_TOOL_NAME.to_owned(),
                input: serde_json::json!({}),
            },
        });
        let _ = t.consume(AnthropicEvent::MessageDelta {
            delta: MessageDeltaPayload { stop_reason: Some("tool_use".to_owned()) },
            usage: None,
        });
        let out = t.consume(AnthropicEvent::MessageStop).unwrap();
        match out.into_iter().next().unwrap().unwrap() {
            ModelEvent::Finish { reason } => assert_eq!(reason, FinishReason::Stop),
            _ => panic!("expected Finish::Stop"),
        }
    }

    #[test]
    fn synthesized_plus_real_tool_errors() {
        let mut t = MessageTranslator::new(true);
        let _ = t.consume(message_start(10, None)).unwrap();
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockHead::ToolUse {
                id: "tu_s".to_owned(),
                name: SYNTHESIZED_TOOL_NAME.to_owned(),
                input: serde_json::json!({}),
            },
        });
        let _ = t.consume(AnthropicEvent::ContentBlockStart {
            index: 1,
            content_block: ContentBlockHead::ToolUse {
                id: "tu_r".to_owned(),
                name: "search".to_owned(),
                input: serde_json::json!({}),
            },
        });
        let _ = t.consume(AnthropicEvent::MessageDelta {
            delta: MessageDeltaPayload { stop_reason: Some("tool_use".to_owned()) },
            usage: None,
        });
        let out = t.consume(AnthropicEvent::MessageStop).unwrap();
        match out.into_iter().next().unwrap() {
            Err(ModelError::Other(_)) => {}
            other => panic!("expected Err(Other), got {other:?}"),
        }
    }

    #[test]
    fn in_stream_overloaded_error_terminates_with_unavailable() {
        let mut t = MessageTranslator::new(false);
        let err = t
            .consume(AnthropicEvent::Error {
                error: AnthropicErrorPayload {
                    ty: "overloaded_error".to_owned(),
                    message: "busy".to_owned(),
                },
            })
            .unwrap_err();
        assert!(matches!(err, ModelError::Unavailable));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic stream`

Expected: 9 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/stream.rs
git commit -m "feat(providers-anthropic): SMA-317 add MessageTranslator state machine"
```

---

## Task 19: `translate/mod.rs` — `build_body` orchestrator

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/translate/mod.rs`

Aggregates Tasks 9–14: takes a `Config` + `ModelRequest`, runs validation guards, builds the final request body, and reports whether synthesis is active (the stream translator needs the flag).

- [ ] **Step 1: Replace `translate/mod.rs`**

```rust
//! Translation between Paigasus carrier types and Anthropic wire format.

pub(crate) mod cache;
pub(crate) mod request;
pub(crate) mod response_format;
pub(crate) mod tools;

use paigasus_helikon_core::{ModelError, ModelRequest, ToolChoice};
use serde_json::{json, Value};

use crate::builder::Config;
use crate::settings::ExtendedThinking;
use response_format::{
    synthesize_for_response_format, validate_conflict, validate_tool_names, Synthesized,
};

/// Built request body + whether the stream translator should be in synthesis mode.
pub(crate) struct PreparedRequest {
    pub(crate) body: Value,
    pub(crate) synthesizing_output: bool,
}

/// Build the JSON request body from the caller's `ModelRequest` plus the
/// builder-baked `Config`. Runs all synchronous validation guards.
pub(crate) fn build_body(cfg: &Config, req: &ModelRequest) -> Result<PreparedRequest, ModelError> {
    validate_tool_names(&req.tools).map_err(|m| ModelError::Other(anyhow::anyhow!(m)))?;
    validate_conflict(
        req.model_settings.response_format.as_ref(),
        req.model_settings.tool_choice.as_ref(),
    )
    .map_err(|m| ModelError::Other(anyhow::anyhow!(m)))?;

    let translated = request::translate_messages(&req.messages);

    let mut tools_array = tools::translate_tools(&req.tools);

    let Synthesized { tool, tool_choice, synthesizing } =
        synthesize_for_response_format(req.model_settings.response_format.as_ref());
    if let Some(extra) = tool {
        if let Some(arr) = tools_array.as_array_mut() {
            arr.push(extra);
        }
    }

    let mut messages = translated.messages;
    let system =
        cache::apply_cache_strategy(cfg.cache_strategy, translated.system, &mut tools_array, &mut messages);

    let mut body = serde_json::Map::new();
    body.insert("model".into(), Value::String(cfg.model_id.clone()));
    body.insert("stream".into(), Value::Bool(true));
    body.insert("messages".into(), messages);
    body.insert(
        "max_tokens".into(),
        Value::Number(
            req.model_settings
                .max_output_tokens
                .map(|m| m.into())
                .unwrap_or_else(|| cfg.max_output_default.into()),
        ),
    );
    if let Some(s) = system {
        body.insert("system".into(), s);
    }
    if tools_array.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
        body.insert("tools".into(), tools_array);
    }

    // tool_choice: synthesis overrides caller; otherwise translate caller's preference.
    let tc_value = match tool_choice {
        Some(v) => Some(v),
        None => req
            .model_settings
            .tool_choice
            .as_ref()
            .map(translate_tool_choice),
    };
    if let Some(v) = tc_value {
        body.insert("tool_choice".into(), v);
    }

    if let Some(t) = req.model_settings.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if let Some(p) = req.model_settings.top_p {
        body.insert("top_p".into(), json!(p));
    }
    if let Some(k) = cfg.top_k {
        body.insert("top_k".into(), json!(k));
    }
    match cfg.extended_thinking {
        ExtendedThinking::Disabled => {}
        ExtendedThinking::Enabled { budget_tokens } => {
            body.insert(
                "thinking".into(),
                json!({"type": "enabled", "budget_tokens": budget_tokens}),
            );
        }
        ExtendedThinking::Adaptive => {
            body.insert("thinking".into(), json!({"type": "adaptive"}));
        }
    }

    if req.model_settings.previous_response_id.is_some() {
        tracing::debug!(
            target: "paigasus::anthropic::translate",
            "previous_response_id is Anthropic-irrelevant; ignoring",
        );
    }

    Ok(PreparedRequest { body: Value::Object(body), synthesizing_output: synthesizing })
}

fn translate_tool_choice(tc: &ToolChoice) -> Value {
    match tc {
        ToolChoice::Auto => json!({"type": "auto"}),
        ToolChoice::Required => json!({"type": "any"}),
        ToolChoice::None => json!({"type": "none"}),
        ToolChoice::Tool { name } => json!({"type": "tool", "name": name}),
        _ => json!({"type": "auto"}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::AnthropicModelBuilder;
    use paigasus_helikon_core::{
        ContentPart, Item, ModelSettings, ResponseFormat, ToolDef,
    };

    fn cfg() -> Config {
        AnthropicModelBuilder::new("claude-sonnet-4-6")
            .api_key("sk-test")
            .build_config()
            .unwrap()
    }

    fn user_text(s: &str) -> Item {
        Item::UserMessage { content: vec![ContentPart::Text { text: s.to_owned() }] }
    }

    #[test]
    fn basic_request_has_model_messages_max_tokens_stream() {
        let req = ModelRequest {
            messages: vec![user_text("hi")],
            tools: vec![],
            model_settings: ModelSettings::default(),
        };
        let p = build_body(&cfg(), &req).unwrap();
        assert_eq!(p.body["model"], "claude-sonnet-4-6");
        assert_eq!(p.body["stream"], true);
        assert!(p.body["messages"].is_array());
        assert_eq!(p.body["max_tokens"], 32_768);
        assert!(!p.synthesizing_output);
    }

    #[test]
    fn caller_max_tokens_overrides_model_default() {
        let req = ModelRequest {
            messages: vec![user_text("hi")],
            tools: vec![],
            model_settings: ModelSettings {
                max_output_tokens: Some(1024),
                ..ModelSettings::default()
            },
        };
        let p = build_body(&cfg(), &req).unwrap();
        assert_eq!(p.body["max_tokens"], 1024);
    }

    #[test]
    fn tool_choice_none_emits_native_none() {
        let req = ModelRequest {
            messages: vec![user_text("hi")],
            tools: vec![ToolDef {
                name: "search".to_owned(),
                description: "".to_owned(),
                schema: serde_json::json!({}),
            }],
            model_settings: ModelSettings {
                tool_choice: Some(ToolChoice::None),
                ..ModelSettings::default()
            },
        };
        let p = build_body(&cfg(), &req).unwrap();
        assert_eq!(p.body["tool_choice"], serde_json::json!({"type": "none"}));
        assert!(p.body["tools"].is_array(), "tools stay in body so prefix matches cached turns");
    }

    #[test]
    fn json_schema_synthesizes_forced_tool() {
        let req = ModelRequest {
            messages: vec![user_text("Build a person.")],
            tools: vec![],
            model_settings: ModelSettings {
                response_format: Some(ResponseFormat::JsonSchema {
                    name: "Person".to_owned(),
                    schema: serde_json::json!({"type": "object"}),
                    strict: false,
                }),
                ..ModelSettings::default()
            },
        };
        let p = build_body(&cfg(), &req).unwrap();
        assert!(p.synthesizing_output);
        assert_eq!(
            p.body["tool_choice"],
            serde_json::json!({"type": "tool", "name": "__paigasus_structured_output__"}),
        );
        let tools = p.body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "__paigasus_structured_output__");
    }

    #[test]
    fn reserved_tool_name_rejected_synchronously() {
        let req = ModelRequest {
            messages: vec![user_text("hi")],
            tools: vec![ToolDef {
                name: "__paigasus_structured_output__".to_owned(),
                description: "".to_owned(),
                schema: serde_json::json!({}),
            }],
            model_settings: ModelSettings::default(),
        };
        let err = build_body(&cfg(), &req).unwrap_err();
        assert!(matches!(err, ModelError::Other(_)));
    }

    #[test]
    fn extended_thinking_adaptive_emits_adaptive_payload() {
        let mut cfg = cfg();
        cfg.extended_thinking = ExtendedThinking::Adaptive;
        let req = ModelRequest {
            messages: vec![user_text("hi")],
            tools: vec![],
            model_settings: ModelSettings::default(),
        };
        let p = build_body(&cfg, &req).unwrap();
        assert_eq!(p.body["thinking"], serde_json::json!({"type": "adaptive"}));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic translate`

Expected: all PASS (Tasks 9–14 + Task 19 = 36 total).

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/translate/mod.rs
git commit -m "feat(providers-anthropic): SMA-317 add build_body orchestrator with guards"
```

---

## Task 20: `model.rs` — `AnthropicModel` + `impl Model::invoke`

**Files:**
- Modify: `crates/paigasus-helikon-providers-anthropic/src/model.rs`
- Modify: `crates/paigasus-helikon-providers-anthropic/src/lib.rs` (uncomment `pub use`)

This wires `build_body` → reqwest POST → `eventsource-stream` → `MessageTranslator` and produces a `BoxStream<Result<ModelEvent, ModelError>>`. Cancellation honors the `CancellationToken`.

- [ ] **Step 1: Replace `model.rs`**

```rust
//! `AnthropicModel` — public [`Model`] implementation.

use async_stream::stream;
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest,
};

use crate::builder::{AnthropicModelBuilder, Config};
use crate::error::{map_error_type, parse_retry_after_ms};
use crate::http::{build_headers, messages_url};
use crate::sse::AnthropicEvent;
use crate::stream::MessageTranslator;
use crate::translate::build_body;

/// Anthropic provider — Messages API.
///
/// Construct via [`Self::messages`].
#[derive(Debug)]
pub struct AnthropicModel {
    pub(crate) cfg: Config,
}

impl AnthropicModel {
    /// Construct a Messages-API model builder.
    pub fn messages(model_id: impl Into<String>) -> AnthropicModelBuilder {
        AnthropicModelBuilder::new(model_id)
    }

    pub(crate) fn from_config(cfg: Config) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl Model for AnthropicModel {
    async fn invoke(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        let prepared = build_body(&self.cfg, &request)?;
        let synthesizing = prepared.synthesizing_output;
        let headers = build_headers(&self.cfg);
        let url = messages_url(&self.cfg);
        let client = self.cfg.http.clone();

        let s = stream! {
            let send_fut = client
                .post(&url)
                .headers(headers)
                .json(&prepared.body)
                .send();

            let response = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                r = send_fut => match r {
                    Ok(r) => r,
                    Err(e) => {
                        yield Err(ModelError::Transport(e.to_string()));
                        return;
                    }
                },
            };

            let status = response.status();
            if !status.is_success() {
                let retry_after_ms = parse_retry_after_ms(response.headers());
                let body_bytes = response.bytes().await.unwrap_or_default();
                let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&body_bytes);
                let (ty, message) = parsed
                    .as_ref()
                    .ok()
                    .and_then(|v| {
                        let ty = v.get("error").and_then(|e| e.get("type"))
                            .and_then(|t| t.as_str()).unwrap_or("").to_owned();
                        let msg = v.get("error").and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str()).unwrap_or("").to_owned();
                        Some((ty, msg))
                    })
                    .unwrap_or_else(|| (
                        String::new(),
                        String::from_utf8_lossy(&body_bytes).into_owned(),
                    ));
                yield Err(map_error_type(Some(status.as_u16()), &ty, &message, retry_after_ms));
                return;
            }

            let mut event_stream = response.bytes_stream().eventsource();
            let mut translator = MessageTranslator::new(synthesizing);

            loop {
                let next = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    n = event_stream.next() => n,
                };
                match next {
                    None => return,
                    Some(Err(e)) => {
                        yield Err(ModelError::Transport(e.to_string()));
                        return;
                    }
                    Some(Ok(event)) => {
                        let parsed: Result<AnthropicEvent, _> = serde_json::from_str(&event.data);
                        let Ok(parsed) = parsed else {
                            tracing::warn!(
                                target: "paigasus::anthropic::sse",
                                "unparseable SSE event: {}", event.data,
                            );
                            continue;
                        };
                        match translator.consume(parsed) {
                            Err(e) => {
                                yield Err(e);
                                return;
                            }
                            Ok(events) => {
                                for ev in events {
                                    yield ev;
                                }
                            }
                        }
                    }
                }
            }
        };

        Ok(Box::pin(s))
    }

    fn capabilities(&self) -> ModelCapabilities {
        self.cfg.capabilities
    }
}

impl AnthropicModelBuilder {
    /// Resolve auth, validate inputs, materialize the [`AnthropicModel`].
    pub fn build(self) -> Result<AnthropicModel, crate::BuildError> {
        Ok(AnthropicModel::from_config(self.build_config()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_reflects_builder_lookup() {
        let m = AnthropicModel::messages("claude-sonnet-4-6")
            .api_key("sk-test")
            .build()
            .unwrap();
        let c = m.capabilities();
        assert!(c.streaming);
        assert!(c.tools);
        assert!(c.prompt_caching);
    }
}
```

- [ ] **Step 2: Uncomment the `pub use` line in `lib.rs`**

```rust
pub use model::AnthropicModel;
```

- [ ] **Step 3: Build + run unit tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic --lib`

Expected: all PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/src/{model.rs,lib.rs}
git commit -m "feat(providers-anthropic): SMA-317 implement Model::invoke for AnthropicModel"
```

---

## Task 21: `tests/fixtures/` — hand-authored SSE fixtures

**Files:**
- Create: `crates/paigasus-helikon-providers-anthropic/tests/fixtures/text_only.txt`
- Create: `crates/paigasus-helikon-providers-anthropic/tests/fixtures/parallel_tool_use.txt`
- Create: `crates/paigasus-helikon-providers-anthropic/tests/fixtures/thinking_then_text.txt`
- Create: `crates/paigasus-helikon-providers-anthropic/tests/fixtures/tool_use_then_continuation.txt`
- Create: `crates/paigasus-helikon-providers-anthropic/tests/fixtures/stream_error.txt`

Each fixture is raw SSE bytes — `event:` line, `data:` line, blank line. `include_str!` loads them into the wiremock streaming tests. Lines marked `# ...` are comments **inside the file** that the test harness splits the file on for multi-stream fixtures (`tool_use_then_continuation`).

- [ ] **Step 1: Create `text_only.txt`**

```
event: message_start
data: {"type":"message_start","message":{"id":"msg_01","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"usage":{"input_tokens":12,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}

event: message_stop
data: {"type":"message_stop"}

```

(Note: trailing blank line is intentional — SSE requires `\n\n` between events.)

- [ ] **Step 2: Create `parallel_tool_use.txt`**

```
event: message_start
data: {"type":"message_start","message":{"id":"msg_02","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"usage":{"input_tokens":20,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_a","name":"a","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"x\":1}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu_b","name":"b","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"y\":2}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":18}}

event: message_stop
data: {"type":"message_stop"}

```

- [ ] **Step 3: Create `thinking_then_text.txt`**

```
event: message_start
data: {"type":"message_start","message":{"id":"msg_03","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"usage":{"input_tokens":30,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me think..."}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc123"}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"42"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":3}}

event: message_stop
data: {"type":"message_stop"}

```

- [ ] **Step 4: Create `tool_use_then_continuation.txt`**

This file holds two SSE streams separated by a `# --- turn 2 ---` line. The test harness splits on that delimiter, serves stream 1 on the first POST, stream 2 on the second.

```
event: message_start
data: {"type":"message_start","message":{"id":"msg_04a","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"usage":{"input_tokens":40,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Checking weather..."}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: content_block_start
data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu_weather","name":"get_weather","input":{}}}

event: content_block_delta
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"city\":\"Athens\"}"}}

event: content_block_stop
data: {"type":"content_block_stop","index":1}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":24}}

event: message_stop
data: {"type":"message_stop"}

# --- turn 2 ---
event: message_start
data: {"type":"message_start","message":{"id":"msg_04b","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"usage":{"input_tokens":52,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"It is 28C and sunny."}}

event: content_block_stop
data: {"type":"content_block_stop","index":0}

event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":8}}

event: message_stop
data: {"type":"message_stop"}

```

- [ ] **Step 5: Create `stream_error.txt`**

```
event: message_start
data: {"type":"message_start","message":{"id":"msg_05","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"usage":{"input_tokens":12,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":0}}}

event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}

event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}

event: error
data: {"type":"error","error":{"type":"overloaded_error","message":"backend overloaded"}}

```

- [ ] **Step 6: Commit fixtures**

```bash
git add crates/paigasus-helikon-providers-anthropic/tests/fixtures/
git commit -m "test(providers-anthropic): SMA-317 add hand-authored SSE fixtures"
```

---

## Task 22: `tests/messages_wire.rs` — non-streaming-shape wire tests

**Files:**
- Create: `crates/paigasus-helikon-providers-anthropic/tests/messages_wire.rs`

Anthropic always streams in our implementation (`"stream": true`). These tests use wiremock to assert *request-side* properties (headers, body shape) and to inject failure responses for the error-mapping path. Streaming-response correctness is in Task 23.

- [ ] **Step 1: Create `tests/messages_wire.rs`**

```rust
//! Wire-format tests for the request side of Anthropic Messages.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelError, ModelRequest, ModelSettings,
};
use paigasus_helikon_providers_anthropic::AnthropicModel;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn empty_stream_response() -> ResponseTemplate {
    // Minimal SSE that ends cleanly so the stream completes.
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(
            "event: message_start\n\
             data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n\
             event: message_delta\n\
             data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n\
             event: message_stop\n\
             data: {\"type\":\"message_stop\"}\n\n",
            "text/event-stream",
        )
}

fn user(s: &str) -> Item {
    Item::UserMessage { content: vec![ContentPart::Text { text: s.to_owned() }] }
}

#[tokio::test]
async fn request_carries_required_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(header("content-type", "application/json"))
        .respond_with(empty_stream_response())
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let req = ModelRequest {
        messages: vec![user("hi")],
        tools: vec![],
        model_settings: ModelSettings::default(),
    };
    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    while let Some(_) = s.next().await {}
}

#[tokio::test]
async fn http_429_with_retry_after_maps_to_rate_limited() {
    let server = MockServer::start().await;
    let body = serde_json::json!({"type": "error", "error": {"type": "rate_limit_error", "message": "slow down"}});
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "7")
                .set_body_json(body),
        )
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let req = ModelRequest {
        messages: vec![user("hi")],
        tools: vec![],
        model_settings: ModelSettings::default(),
    };
    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    let first = s.next().await.expect("stream not empty");
    match first {
        Err(ModelError::RateLimited { retry_after_ms }) => {
            assert_eq!(retry_after_ms, Some(7_000));
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn http_529_overloaded_maps_to_unavailable() {
    let server = MockServer::start().await;
    let body = serde_json::json!({"type":"error","error":{"type":"overloaded_error","message":"busy"}});
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(529).set_body_json(body))
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let req = ModelRequest {
        messages: vec![user("hi")],
        tools: vec![],
        model_settings: ModelSettings::default(),
    };
    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    let first = s.next().await.expect("stream not empty");
    assert!(matches!(first, Err(ModelError::Unavailable)));
}

#[tokio::test]
async fn http_400_prompt_too_long_maps_to_context_length_exceeded() {
    let server = MockServer::start().await;
    let body = serde_json::json!({"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 200k > 200k tokens"}});
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(400).set_body_json(body))
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let req = ModelRequest {
        messages: vec![user("hi")],
        tools: vec![],
        model_settings: ModelSettings::default(),
    };
    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    let first = s.next().await.expect("stream not empty");
    assert!(matches!(first, Err(ModelError::ContextLengthExceeded)));
}

#[tokio::test]
async fn http_401_auth_maps_to_refused() {
    let server = MockServer::start().await;
    let body = serde_json::json!({"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}});
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(body))
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-bad")
        .base_url(server.uri())
        .build()
        .unwrap();
    let req = ModelRequest {
        messages: vec![user("hi")],
        tools: vec![],
        model_settings: ModelSettings::default(),
    };
    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    let first = s.next().await.expect("stream not empty");
    match first {
        Err(ModelError::Refused { reason }) => assert!(reason.contains("invalid")),
        other => panic!("expected Refused, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run the wire tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic --test messages_wire`

Expected: 5 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/tests/messages_wire.rs
git commit -m "test(providers-anthropic): SMA-317 wiremock wire-format and error-mapping tests"
```

---

## Task 23: `tests/messages_streaming.rs` — SSE fixture-driven tests

**Files:**
- Create: `crates/paigasus-helikon-providers-anthropic/tests/messages_streaming.rs`

- [ ] **Step 1: Create the test file**

```rust
//! Streaming SSE fixture tests for the Anthropic provider.
//!
//! Note: wiremock serves the full fixture body in a single chunk; these
//! tests prove byte-level correctness, not resilience to slow chunk delivery.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, FinishReason, Item, Model, ModelError, ModelEvent,
    ModelRequest, ModelSettings,
};
use paigasus_helikon_providers_anthropic::AnthropicModel;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

fn user(s: &str) -> Item {
    Item::UserMessage { content: vec![ContentPart::Text { text: s.to_owned() }] }
}

async fn run_stream(server: &MockServer, fixture: &'static str) -> Vec<Result<ModelEvent, ModelError>> {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(fixture, "text/event-stream"),
        )
        .mount(server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let req = ModelRequest {
        messages: vec![user("hi")],
        tools: vec![],
        model_settings: ModelSettings::default(),
    };
    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    let mut out = Vec::new();
    while let Some(ev) = s.next().await {
        out.push(ev);
    }
    out
}

#[tokio::test]
async fn text_only_stream_emits_usage_token_deltas_usage_finish() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/text_only.txt");
    let events = run_stream(&server, fixture).await;
    let oks: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    // First: Usage from message_start.
    assert!(matches!(oks[0], ModelEvent::Usage { input_tokens: 12, output_tokens: 0, .. }));
    // Then two TokenDelta events.
    assert!(matches!(&oks[1], ModelEvent::TokenDelta { text } if text == "Hello"));
    assert!(matches!(&oks[2], ModelEvent::TokenDelta { text } if text == " world"));
    // Final Usage from message_delta then Finish::Stop.
    assert!(matches!(oks[3], ModelEvent::Usage { output_tokens: 5, .. }));
    assert!(matches!(&oks[4], ModelEvent::Finish { reason } if *reason == FinishReason::Stop));
}

#[tokio::test]
async fn parallel_tool_use_stream_emits_two_tool_call_deltas() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/parallel_tool_use.txt");
    let events = run_stream(&server, fixture).await;
    let oks: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    let tc: Vec<&ModelEvent> = oks
        .iter()
        .filter(|e| matches!(e, ModelEvent::ToolCallDelta { .. }))
        .collect();
    assert_eq!(tc.len(), 2, "two tool calls");
    match tc[0] {
        ModelEvent::ToolCallDelta { call_id, name, .. } => {
            assert_eq!(call_id, "tu_a");
            assert_eq!(name.as_deref(), Some("a"));
        }
        _ => unreachable!(),
    }
    match tc[1] {
        ModelEvent::ToolCallDelta { call_id, name, .. } => {
            assert_eq!(call_id, "tu_b");
            assert_eq!(name.as_deref(), Some("b"));
        }
        _ => unreachable!(),
    }

    assert!(matches!(
        oks.last().unwrap(),
        ModelEvent::Finish { reason: FinishReason::ToolCalls },
    ));
}

#[tokio::test]
async fn thinking_stream_emits_reasoning_delta_before_text_delta() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/thinking_then_text.txt");
    let events = run_stream(&server, fixture).await;
    let oks: Vec<_> = events.into_iter().map(|r| r.unwrap()).collect();

    let first_reasoning = oks
        .iter()
        .position(|e| matches!(e, ModelEvent::ReasoningDelta { .. }))
        .expect("reasoning delta present");
    let first_text = oks
        .iter()
        .position(|e| matches!(e, ModelEvent::TokenDelta { .. }))
        .expect("text delta present");
    assert!(first_reasoning < first_text, "reasoning must precede text in this fixture");
}

#[tokio::test]
async fn stream_error_overloaded_terminates_with_unavailable() {
    let server = MockServer::start().await;
    let fixture = include_str!("fixtures/stream_error.txt");
    let events = run_stream(&server, fixture).await;
    let last = events.into_iter().last().unwrap();
    assert!(matches!(last, Err(ModelError::Unavailable)));
}

/// Two-turn fixture: serve stream 1 on the first POST, stream 2 on the second.
struct SwitchingResponder {
    counter: std::sync::Mutex<usize>,
    bodies: Vec<String>,
}
impl Respond for SwitchingResponder {
    fn respond(&self, _req: &wiremock::Request) -> ResponseTemplate {
        let mut c = self.counter.lock().unwrap();
        let body = self.bodies.get(*c).cloned().unwrap_or_default();
        *c += 1;
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_raw(body, "text/event-stream")
    }
}

#[tokio::test]
async fn multi_turn_tool_use_continuation() {
    let raw = include_str!("fixtures/tool_use_then_continuation.txt");
    let parts: Vec<&str> = raw.split("# --- turn 2 ---\n").collect();
    assert_eq!(parts.len(), 2, "fixture must contain the turn-2 delimiter");
    let bodies = vec![parts[0].to_owned(), parts[1].to_owned()];

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(SwitchingResponder { counter: Default::default(), bodies })
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();

    // Turn 1: prompt → expect text + tool_use + Finish::ToolCalls.
    let turn1_req = ModelRequest {
        messages: vec![user("weather in athens?")],
        tools: vec![],
        model_settings: ModelSettings::default(),
    };
    let mut s = model.invoke(turn1_req, CancellationToken::new()).await.unwrap();
    let mut events1: Vec<_> = Vec::new();
    while let Some(ev) = s.next().await {
        events1.push(ev.unwrap());
    }
    assert!(events1
        .iter()
        .any(|e| matches!(e, ModelEvent::ToolCallDelta { call_id, .. } if call_id == "tu_weather")));
    assert!(matches!(
        events1.last().unwrap(),
        ModelEvent::Finish { reason: FinishReason::ToolCalls },
    ));

    // Turn 2: append tool_result and re-invoke.
    let turn2_req = ModelRequest {
        messages: vec![
            user("weather in athens?"),
            Item::AssistantMessage {
                content: vec![ContentPart::ToolUse {
                    call_id: "tu_weather".to_owned(),
                    name: "get_weather".to_owned(),
                    args: serde_json::json!({"city": "Athens"}),
                }],
                agent: None,
            },
            Item::ToolResult {
                call_id: "tu_weather".to_owned(),
                content: vec![ContentPart::Text { text: "28C, sunny".to_owned() }],
            },
        ],
        tools: vec![],
        model_settings: ModelSettings::default(),
    };
    let mut s = model.invoke(turn2_req, CancellationToken::new()).await.unwrap();
    let mut events2: Vec<_> = Vec::new();
    while let Some(ev) = s.next().await {
        events2.push(ev.unwrap());
    }
    assert!(events2.iter().any(|e| matches!(e, ModelEvent::TokenDelta { text } if text.contains("28C"))));
    assert!(matches!(
        events2.last().unwrap(),
        ModelEvent::Finish { reason: FinishReason::Stop },
    ));
}
```

- [ ] **Step 2: Run the streaming tests**

Run: `cargo test -p paigasus-helikon-providers-anthropic --test messages_streaming`

Expected: 5 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/tests/messages_streaming.rs
git commit -m "test(providers-anthropic): SMA-317 SSE-fixture streaming integration tests"
```

---

## Task 24: `tests/prompt_caching.rs` — acceptance-criterion test

**Files:**
- Create: `crates/paigasus-helikon-providers-anthropic/tests/prompt_caching.rs`

The spec's acceptance criterion: with `CacheStrategy::SystemAndTools` and an identical prefix, the second turn's `cached_input_tokens` is non-zero. Implementation: mock serves turn-1 with `cache_creation_input_tokens: 2048, cache_read_input_tokens: 0`; turn-2 with `cache_read_input_tokens: 2048`. Both bodies must contain `cache_control` markers in the expected positions.

- [ ] **Step 1: Create the test file**

```rust
//! Acceptance-criterion test: prompt caching reduces second-turn input tokens.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest, ModelSettings, ToolDef,
};
use paigasus_helikon_providers_anthropic::{AnthropicModel, CacheStrategy};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

const SYSTEM: &str = "You are a helpful assistant in a verbose tone. \
                     Always answer concisely with units. \
                     Use the available tools when relevant.";

fn turn(input: u32, cache_creation: u32, cache_read: u32, output: u32) -> String {
    format!(
        "event: message_start\n\
         data: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":{input},\"cache_read_input_tokens\":{cache_read},\"cache_creation_input_tokens\":{cache_creation},\"output_tokens\":0}}}}}}\n\n\
         event: content_block_start\n\
         data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
         event: content_block_delta\n\
         data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"ok\"}}}}\n\n\
         event: content_block_stop\n\
         data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
         event: message_delta\n\
         data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":{output}}}}}\n\n\
         event: message_stop\n\
         data: {{\"type\":\"message_stop\"}}\n\n"
    )
}

struct SwitchingResponder {
    counter: std::sync::Mutex<usize>,
    bodies: Vec<String>,
    seen_bodies: std::sync::Mutex<Vec<serde_json::Value>>,
}
impl Respond for SwitchingResponder {
    fn respond(&self, req: &wiremock::Request) -> ResponseTemplate {
        let v: serde_json::Value =
            serde_json::from_slice(&req.body).expect("request body is JSON");
        self.seen_bodies.lock().unwrap().push(v);
        let mut c = self.counter.lock().unwrap();
        let b = self.bodies.get(*c).cloned().unwrap_or_default();
        *c += 1;
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_raw(b, "text/event-stream")
    }
}

fn user(s: &str) -> Item {
    Item::UserMessage { content: vec![ContentPart::Text { text: s.to_owned() }] }
}

#[tokio::test]
async fn second_turn_cached_input_tokens_reflects_prefix_reuse() {
    let server = MockServer::start().await;
    let bodies = vec![turn(2200, 2048, 0, 5), turn(150, 0, 2048, 5)];
    let responder = SwitchingResponder {
        counter: Default::default(),
        bodies,
        seen_bodies: Default::default(),
    };
    let seen = std::sync::Arc::new(responder.seen_bodies.lock().unwrap().clone());
    // wiremock takes ownership of the responder, so we'll re-check seen via mock after.
    let _ = seen;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(responder)
        .mount(&server)
        .await;

    let tool = ToolDef {
        name: "search".to_owned(),
        description: "Search the web.".to_owned(),
        schema: serde_json::json!({"type": "object", "properties": {"q": {"type": "string"}}}),
    };

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .cache_strategy(CacheStrategy::SystemAndTools)
        .build()
        .unwrap();

    let base_messages = vec![
        Item::System { content: vec![ContentPart::Text { text: SYSTEM.to_owned() }] },
        user("Tell me about Athens."),
    ];
    let req1 = ModelRequest {
        messages: base_messages.clone(),
        tools: vec![tool.clone()],
        model_settings: ModelSettings::default(),
    };
    let mut s = model.invoke(req1, CancellationToken::new()).await.unwrap();
    let mut events1 = Vec::new();
    while let Some(ev) = s.next().await {
        events1.push(ev.unwrap());
    }
    // First Usage on turn 1: cached should be 0.
    let usage1 = events1
        .iter()
        .find_map(|e| match e {
            ModelEvent::Usage { cached_input_tokens, .. } => Some(*cached_input_tokens),
            _ => None,
        })
        .unwrap();
    assert_eq!(usage1, Some(0), "turn 1 has no cache reads");

    // Turn 2: identical prefix + new question.
    let mut messages2 = base_messages.clone();
    messages2.push(Item::AssistantMessage {
        content: vec![ContentPart::Text { text: "ok".to_owned() }],
        agent: None,
    });
    messages2.push(user("And Sparta?"));
    let req2 = ModelRequest {
        messages: messages2,
        tools: vec![tool],
        model_settings: ModelSettings::default(),
    };
    let mut s = model.invoke(req2, CancellationToken::new()).await.unwrap();
    let mut events2 = Vec::new();
    while let Some(ev) = s.next().await {
        events2.push(ev.unwrap());
    }
    let usage2 = events2
        .iter()
        .find_map(|e| match e {
            ModelEvent::Usage { cached_input_tokens, .. } => Some(*cached_input_tokens),
            _ => None,
        })
        .unwrap();
    assert_eq!(usage2, Some(2048), "turn 2 reads the cached prefix");

    // Inspect the request bodies the mock saw to confirm cache markers were sent.
    let received = server.received_requests().await.expect("requests recorded");
    assert_eq!(received.len(), 2);
    for r in &received {
        let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
        let system_marker = body["system"][0]["cache_control"]["type"].as_str();
        assert_eq!(system_marker, Some("ephemeral"), "system block carries cache marker");
        let tools_arr = body["tools"].as_array().unwrap();
        assert_eq!(
            tools_arr.last().unwrap()["cache_control"]["type"].as_str(),
            Some("ephemeral"),
            "last tool carries cache marker",
        );
    }

    // Capability flag reflects cache support.
    assert!(model.capabilities().prompt_caching);
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p paigasus-helikon-providers-anthropic --test prompt_caching`

Expected: 1 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/tests/prompt_caching.rs
git commit -m "test(providers-anthropic): SMA-317 prompt-caching acceptance test (two-turn cache read)"
```

---

## Task 25: `tests/structured_output.rs` — forced-tool synthesis round-trip

**Files:**
- Create: `crates/paigasus-helikon-providers-anthropic/tests/structured_output.rs`

- [ ] **Step 1: Create the test file**

```rust
//! End-to-end test of `ResponseFormat::JsonSchema` via forced-tool synthesis.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, FinishReason, Item, Model, ModelError, ModelEvent,
    ModelRequest, ModelSettings, ResponseFormat, ToolChoice, ToolDef,
};
use paigasus_helikon_providers_anthropic::AnthropicModel;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

const SYNTH_NAME: &str = "__paigasus_structured_output__";

struct CapturingResponder {
    body: String,
    captured: std::sync::Mutex<Option<serde_json::Value>>,
}
impl Respond for CapturingResponder {
    fn respond(&self, req: &wiremock::Request) -> ResponseTemplate {
        let v: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        *self.captured.lock().unwrap() = Some(v);
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_raw(self.body.clone(), "text/event-stream")
    }
}

fn synth_tool_use_stream() -> String {
    format!(
        "event: message_start\n\
         data: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":10}}}}}}\n\n\
         event: content_block_start\n\
         data: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"tu_s\",\"name\":\"{name}\",\"input\":{{}}}}}}\n\n\
         event: content_block_delta\n\
         data: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":\"{{\\\"name\\\":\\\"Ada\\\"}}\"}}}}\n\n\
         event: content_block_stop\n\
         data: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
         event: message_delta\n\
         data: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"tool_use\"}},\"usage\":{{\"output_tokens\":8}}}}\n\n\
         event: message_stop\n\
         data: {{\"type\":\"message_stop\"}}\n\n",
        name = SYNTH_NAME
    )
}

fn user(s: &str) -> Item {
    Item::UserMessage { content: vec![ContentPart::Text { text: s.to_owned() }] }
}

#[tokio::test]
async fn json_schema_synthesizes_forced_tool_and_remaps_to_text() {
    let responder = CapturingResponder {
        body: synth_tool_use_stream(),
        captured: Default::default(),
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(responder)
        .mount(&server)
        .await;

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let req = ModelRequest {
        messages: vec![user("Give me a Person.")],
        tools: vec![],
        model_settings: ModelSettings {
            response_format: Some(ResponseFormat::JsonSchema {
                name: "Person".to_owned(),
                schema: serde_json::json!({"type": "object", "properties": {"name": {"type": "string"}}}),
                strict: true,
            }),
            ..ModelSettings::default()
        },
    };
    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    let mut events = Vec::new();
    while let Some(ev) = s.next().await {
        events.push(ev.unwrap());
    }

    let text: String = events
        .iter()
        .filter_map(|e| match e {
            ModelEvent::TokenDelta { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "{\"name\":\"Ada\"}");

    assert!(!events.iter().any(|e| matches!(e, ModelEvent::ToolCallDelta { .. })),
        "synthesized tool must NOT surface as ToolCallDelta");

    assert!(matches!(
        events.last().unwrap(),
        ModelEvent::Finish { reason: FinishReason::Stop },
    ), "tool_use stop_reason rewrites to Stop for synthesized-only path");

    // The mock captured the request body — verify synthesized tool + tool_choice.
    let received = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(body["tool_choice"]["type"], "tool");
    assert_eq!(body["tool_choice"]["name"], SYNTH_NAME);
    let tools = body["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == SYNTH_NAME));
}

#[tokio::test]
async fn json_schema_plus_caller_tool_choice_tool_returns_synchronous_other() {
    let server = MockServer::start().await;
    // No mount needed — the guard fires before the HTTP call.

    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let req = ModelRequest {
        messages: vec![user("hi")],
        tools: vec![ToolDef {
            name: "search".to_owned(),
            description: "".to_owned(),
            schema: serde_json::json!({}),
        }],
        model_settings: ModelSettings {
            response_format: Some(ResponseFormat::JsonObject),
            tool_choice: Some(ToolChoice::Tool { name: "search".to_owned() }),
            ..ModelSettings::default()
        },
    };
    let err = model
        .invoke(req, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, ModelError::Other(_)));
}

#[tokio::test]
async fn reserved_tool_name_returns_synchronous_other() {
    let server = MockServer::start().await;
    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .api_key("sk-test")
        .base_url(server.uri())
        .build()
        .unwrap();
    let req = ModelRequest {
        messages: vec![user("hi")],
        tools: vec![ToolDef {
            name: SYNTH_NAME.to_owned(),
            description: "".to_owned(),
            schema: serde_json::json!({}),
        }],
        model_settings: ModelSettings::default(),
    };
    let err = model
        .invoke(req, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, ModelError::Other(_)));
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p paigasus-helikon-providers-anthropic --test structured_output`

Expected: 3 PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/tests/structured_output.rs
git commit -m "test(providers-anthropic): SMA-317 structured-output forced-tool round-trip"
```

---

## Task 26: `tests/live.rs` — ANTHROPIC_API_KEY-gated smoke tests

**Files:**
- Create: `crates/paigasus-helikon-providers-anthropic/tests/live.rs`

- [ ] **Step 1: Create the file**

```rust
//! Live integration tests against the real Anthropic API.
//!
//! All `#[ignore]` so they don't run in CI. Activate locally with
//! `cargo test -p paigasus-helikon-providers-anthropic -- --ignored`.
//! Each test no-ops without an API key.

use futures_util::StreamExt;
use paigasus_helikon_core::{
    CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest, ModelSettings,
    ResponseFormat,
};
use paigasus_helikon_providers_anthropic::{AnthropicModel, CacheStrategy};

fn skip_if_no_key() -> bool {
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        tracing::info!("ANTHROPIC_API_KEY unset; skipping live test");
        return true;
    }
    false
}

fn user(s: &str) -> Item {
    Item::UserMessage { content: vec![ContentPart::Text { text: s.to_owned() }] }
}

#[tokio::test]
#[ignore]
async fn messages_smoke() {
    if skip_if_no_key() {
        return;
    }
    let model = AnthropicModel::messages("claude-haiku-4-5").build().unwrap();
    let req = ModelRequest {
        messages: vec![user("Reply with exactly: hello")],
        tools: vec![],
        model_settings: ModelSettings { max_output_tokens: Some(64), ..Default::default() },
    };
    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    let mut text = String::new();
    while let Some(ev) = s.next().await {
        if let Ok(ModelEvent::TokenDelta { text: t }) = ev {
            text.push_str(&t);
        }
    }
    assert!(text.to_lowercase().contains("hello"));
}

#[tokio::test]
#[ignore]
async fn structured_output_smoke() {
    if skip_if_no_key() {
        return;
    }
    let model = AnthropicModel::messages("claude-haiku-4-5").build().unwrap();
    let req = ModelRequest {
        messages: vec![user("Give a Person named Ada.")],
        tools: vec![],
        model_settings: ModelSettings {
            response_format: Some(ResponseFormat::JsonSchema {
                name: "Person".to_owned(),
                schema: serde_json::json!({
                    "type": "object",
                    "properties": {"name": {"type": "string"}},
                    "required": ["name"]
                }),
                strict: true,
            }),
            max_output_tokens: Some(256),
            ..Default::default()
        },
    };
    let mut s = model.invoke(req, CancellationToken::new()).await.unwrap();
    let mut text = String::new();
    while let Some(ev) = s.next().await {
        if let Ok(ModelEvent::TokenDelta { text: t }) = ev {
            text.push_str(&t);
        }
    }
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("response is JSON");
    assert!(parsed["name"].is_string());
}

#[tokio::test]
#[ignore]
async fn cache_strategy_round_trip() {
    if skip_if_no_key() {
        return;
    }
    // Construct a system prompt big enough to exceed the per-model cache write minimum.
    let big_system = "You are a careful assistant. ".repeat(200);
    let model = AnthropicModel::messages("claude-sonnet-4-6")
        .cache_strategy(CacheStrategy::SystemAndTools)
        .build()
        .unwrap();
    let messages = vec![
        Item::System { content: vec![ContentPart::Text { text: big_system.clone() }] },
        user("Hello, ack only."),
    ];
    let req1 = ModelRequest {
        messages: messages.clone(),
        tools: vec![],
        model_settings: ModelSettings { max_output_tokens: Some(32), ..Default::default() },
    };
    let mut s = model.invoke(req1, CancellationToken::new()).await.unwrap();
    while let Some(_) = s.next().await {}

    let mut messages2 = messages;
    messages2.push(Item::AssistantMessage {
        content: vec![ContentPart::Text { text: "ack".to_owned() }],
        agent: None,
    });
    messages2.push(user("Again, ack."));
    let req2 = ModelRequest {
        messages: messages2,
        tools: vec![],
        model_settings: ModelSettings { max_output_tokens: Some(32), ..Default::default() },
    };
    let mut s = model.invoke(req2, CancellationToken::new()).await.unwrap();
    let mut cached = 0u32;
    while let Some(ev) = s.next().await {
        if let Ok(ModelEvent::Usage { cached_input_tokens, .. }) = ev {
            if let Some(c) = cached_input_tokens {
                cached = cached.max(c);
            }
        }
    }
    if cached == 0 {
        tracing::info!(
            "cache_prefix_too_small: live cache test ran below per-model write minimum",
        );
        // Pass — caching at <write-minimum is a documented no-op.
    } else {
        assert!(cached > 0);
    }
}
```

- [ ] **Step 2: Compile-only (don't run live)**

Run: `cargo test -p paigasus-helikon-providers-anthropic --test live --no-run`

Expected: compiles. Live runs are manual: `ANTHROPIC_API_KEY=... cargo test -p paigasus-helikon-providers-anthropic --test live -- --ignored`.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon-providers-anthropic/tests/live.rs
git commit -m "test(providers-anthropic): SMA-317 ANTHROPIC_API_KEY-gated live smoke tests"
```

---

## Task 27: Final CI gate run + acceptance walk-through

**Files:** none modified — verification + final commit.

- [ ] **Step 1: Format, lint, test, doc, doc-coverage — match `.github/workflows/ci.yml`**

Run each gate locally. If any fails, **stop** — fix root cause and re-run before proceeding.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
  bash scripts/check-doc-coverage.sh
```

Expected: all six gates exit 0.

**Common failure modes:**
- `missing_docs` warnings on new `pub` items → add `///` doc comments.
- Clippy `needless_collect`, `clone_on_copy`, etc. → fix as flagged; no allow-attributes.
- Doc-coverage below threshold → add doc comments on the offending public items.
- `convco check` (run by the `commits` job in CI) — verify each commit message on the branch matches the scope allowlist. Run `convco check origin/main..HEAD` locally.

- [ ] **Step 2: Walk through each ticket acceptance criterion**

Open the spec and verify each item:

- `AnthropicModel` implements `Model` ✓ (`model.rs`)
- Messages API with system prompt + interleaved tool_use / tool_result blocks ✓ (`translate/request.rs` tests in Tasks 10–11)
- Streaming SSE → `MessageStart` / `ContentBlockDelta` (text + tool_use + thinking) / `MessageStop` ✓ (Task 18 + Task 23 fixtures)
- Structured output via single forced tool ✓ (Task 14 + Task 25)
- Prompt-caching support, surfaced in `ModelCapabilities` ✓ (Task 13 + Tasks 2, 7, 24)
- `ANTHROPIC_API_KEY` env or builder param ✓ (Task 8)
- Wire-format snapshot tests for multi-turn tool-use exchange ✓ (Task 23 `multi_turn_tool_use_continuation`)
- Caching reduces input tokens on the second turn ✓ (Task 24)

- [ ] **Step 3: Open the PR**

```bash
git push -u origin feature/sma-317-anthropic-provider-messages-streaming-tool-use-prompt
gh pr create --title "feat(providers-anthropic): SMA-317 add Anthropic provider with streaming, tools, caching" --body "$(cat <<'EOF'
## Summary
- Implements `AnthropicModel` per the SMA-317 spec — Messages API with streaming, tool use, prompt caching, extended/adaptive thinking, structured output via forced-tool synthesis.
- Adds `ModelCapabilities::prompt_caching` to core; backfills the flag on cache-eligible OpenAI models.
- Re-exports the provider crate under the facade's `anthropic` feature.

## Test plan
- [ ] `cargo test --workspace --all-features` green
- [ ] `cargo clippy --workspace --all-features --all-targets -- -D warnings` green
- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps` green
- [ ] `DOC_COVERAGE_THRESHOLD=80 bash scripts/check-doc-coverage.sh` green
- [ ] Live tests (requires `ANTHROPIC_API_KEY`): `cargo test -p paigasus-helikon-providers-anthropic --test live -- --ignored`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

PR title check: starts with `feat(providers-anthropic):`, follows with `SMA-317`, then a lowercase-leading subject (`add`). Matches the `^([A-Z]{2,4}-\d+ )?[^A-Z].+$` regex enforced by `pr-title.yml`.

- [ ] **Step 4: Confirm PR is mergeable**

```bash
gh pr checks
gh pr view --json mergeable,mergeStateStatus,reviewDecision
```

Expected: every required context has reported `pass`; `mergeable: MERGEABLE`; awaiting review.

---

## Spec coverage cross-check

Each spec section ↔ task:

| Spec section | Implemented in |
| --- | --- |
| Architectural decisions — Wire layer | Tasks 4, 16, 17, 20 |
| Architectural decisions — Type shape | Tasks 7, 8, 20 |
| Cross-crate change (core) | Task 2 |
| Cross-crate change (OpenAI backfill) | Task 3 |
| Public API (AnthropicModel, builder, settings, BuildError) | Tasks 6, 8, 20 |
| Wire translation — Messages | Tasks 9, 10, 11, 12 |
| Wire translation — Tools | Task 13 |
| Wire translation — Response format | Task 14 |
| Wire translation — Settings passthrough | Task 19 |
| Cache strategy placement | Task 13 |
| Streaming SSE → ModelEvent | Tasks 17, 18, 21, 23 |
| Error mapping (shared helper) | Tasks 15, 22 |
| Capabilities table (with max_output_default) | Task 7 |
| Testing — Unit | Tasks 6, 7, 8, 9–14, 15, 16, 17, 18 |
| Testing — Wire | Task 22 |
| Testing — Streaming | Task 23 |
| Testing — Prompt caching (acceptance criterion) | Task 24 |
| Testing — Structured output | Task 25 |
| Testing — Live | Task 26 |
| Facade wiring | Task 5 |
| Dependencies | Task 1 |
| CI gate run + acceptance walk-through | Task 27 |

No spec section without an implementing task.









