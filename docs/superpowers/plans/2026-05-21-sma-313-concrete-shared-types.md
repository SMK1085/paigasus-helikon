# SMA-313 — Concrete shared types implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fill in the data plane that the seven SMA-312 traits exchange — `Item`, `AgentEvent` (full 14-variant ADT), `RunContext<Ctx>`, `ToolContext<Ctx>`, `RunResult<T = String>`, plus supporting `TokenUsage`, `HookRegistry<Ctx>`, `TracerHandle` — and migrate `SessionEvent` to carry `Vec<ContentPart>` so the wire format is end-to-end coherent.

**Architecture:** API shape only — no behavior beyond serde round-trip. New `item.rs` module hosts the canonical wire format (`Item`, `ContentPart`, `MediaSource`); the four other domain modules (`agent`, `runner`, `context`, `tool`, `session`, `guardrail`) graduate from placeholders. `Runner::run`'s signature is unchanged — `RunResult<T = String>` keeps object-safety via the default type parameter. Structured-output callers go through `RunResult::<String>::parse_final::<T>()`.

**Tech Stack:** Rust 1.75 (MSRV), `serde` 1, `serde_json` 1, `schemars` 1 (already pinned by SMA-304), `insta` 1 (new workspace pin), plus the existing async stack from SMA-312 (`async-trait`, `thiserror`, `anyhow`, `futures-core`, `tokio-util`).

**Spec:** [`docs/superpowers/specs/2026-05-21-sma-313-concrete-shared-types-design.md`](../specs/2026-05-21-sma-313-concrete-shared-types-design.md).

**Branch:** `feature/sma-313-concrete-shared-types-item-agentevent-runcontext-runresult` (already created; spec commits live there).

---

## Pre-flight

Verify branch state before starting:

```bash
git status
# Expected: On branch feature/sma-313-concrete-shared-types-item-agentevent-runcontext-runresult
#           nothing to commit, working tree clean

git log --oneline -3
# Expected (most recent first):
# 7a5b0c5 docs(spec): SMA-313 correct schemars pin reference
# d67cf1d docs(spec): SMA-313 add design for concrete shared types
# 6dee862 feat(core): SMA-312 define core trait surface (#17)
```

The crate state after SMA-312:

```
crates/paigasus-helikon-core/src/
├── agent.rs        # 4-variant AgentEvent stub
├── context.rs      # PhantomData-only RunContext
├── guardrail.rs    # GuardrailKind without serde derives
├── hook.rs
├── item.rs         # DOES NOT EXIST — created in Task 2
├── lib.rs          # 8 modules re-exported flat
├── model.rs
├── runner.rs       # unit-like RunResult, no TokenUsage
├── session.rs      # SessionEvent::UserMessage { text: String }, etc.
└── tool.rs         # PhantomData-only ToolContext

crates/paigasus-helikon-core/tests/
└── object_safety.rs   # receives RunContext<()> / ToolContext<()> as method params only
```

After each task's commit, run the matching local CI gate to catch issues early:

```bash
cargo fmt --all -- --check
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
cargo test -p paigasus-helikon-core --all-features
cargo doc -p paigasus-helikon-core --all-features --no-deps           # with RUSTDOCFLAGS="-D warnings"
```

Task 11 re-runs the full workspace gate.

**Insta workflow note:** Snapshot tests fail on first run because no `.snap` files exist yet. Re-run that test command with `INSTA_UPDATE=auto` prepended to auto-accept new snapshots; the tool writes `.snap` files into `tests/snapshots/`. After regeneration, run the command again without the env var to confirm the round-trip equality assertions hold. Commit the `.snap` files alongside the test source.

---

## Task 1: Wire `schemars` and `insta` into `paigasus-helikon-core`

**Files:**
- Modify: `Cargo.toml` (workspace root, `[workspace.dependencies]`)
- Modify: `crates/paigasus-helikon-core/Cargo.toml`

`schemars = "1"` is already pinned in workspace deps from the SMA-304 bootstrap. This task adds `insta = "1"` to workspace deps and wires both crates into `paigasus-helikon-core/Cargo.toml`.

- [ ] **Step 1: Add `insta` to workspace dependencies**

Edit `Cargo.toml` at the repo root. Add one line to the third-party block of `[workspace.dependencies]`, immediately after `tokio-util`:

```toml
insta         = "1"
```

The third-party block after the edit looks like:

```toml
[workspace.dependencies]
serde         = { version = "1", features = ["derive"] }
serde_json    = "1"
schemars      = "1"
tokio         = { version = "1", features = ["full"] }
tracing       = "0.1"
opentelemetry = "0.27"
rmcp          = "0.16"
thiserror     = "2"
anyhow        = "1"
async-trait   = "0.1"
futures-core  = "0.3"
tokio-util    = { version = "0.7", default-features = false, features = ["rt"] }
insta         = "1"
```

- [ ] **Step 2: Add `schemars` runtime dep and dev-deps to the crate manifest**

Replace `crates/paigasus-helikon-core/Cargo.toml` with the following. The change is two lines in `[dependencies]` (adding `schemars`) and a new `[dev-dependencies]` block:

```toml
[package]
name        = "paigasus-helikon-core"
description = "Trait surface and concrete types for the Paigasus Helikon AI SDK."
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
async-trait  = { workspace = true }
thiserror    = { workspace = true }
anyhow       = { workspace = true }
serde        = { workspace = true }
serde_json   = { workspace = true }
futures-core = { workspace = true }
tokio-util   = { workspace = true }
schemars     = { workspace = true }

[dev-dependencies]
insta        = { workspace = true, features = ["yaml", "json"] }
schemars     = { workspace = true }
serde_json   = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 3: Verify the workspace resolves and the crate still compiles**

```bash
cargo check -p paigasus-helikon-core
```

Expected: exits 0. `cargo` may download new crates (`insta` and its deps) on first invocation — that's expected.

- [ ] **Step 4: Verify the full workspace still compiles and tests pass**

```bash
cargo test --workspace --all-features
```

Expected: exits 0. The existing `tests/object_safety.rs` from SMA-312 keeps passing.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/paigasus-helikon-core/Cargo.toml
git commit -m "$(cat <<'EOF'
chore(deps): SMA-313 wire schemars and insta into core crate

Adds insta = "1" to [workspace.dependencies] and wires both schemars
(already pinned by SMA-304) and insta into paigasus-helikon-core's
[dependencies] / [dev-dependencies] in preparation for landing the
data-plane types.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Add `Item`, `ContentPart`, `MediaSource` + round-trip tests

**Files:**
- Create: `crates/paigasus-helikon-core/src/item.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs`
- Create: `crates/paigasus-helikon-core/tests/serde_roundtrip.rs`
- Create: `crates/paigasus-helikon-core/tests/snapshots/` (auto-generated by `insta`)

- [ ] **Step 1: Write the failing test file**

Create `crates/paigasus-helikon-core/tests/serde_roundtrip.rs` with the 13 tests covering `Item` (5), `ContentPart` (6), and `MediaSource` (2). The file will fail to compile until the types exist (Step 3):

```rust
//! Locks AC #1 — every serializable variant round-trips through JSON.
//!
//! Each test serializes a representative instance, snapshots the prettified
//! JSON, deserializes it back, and re-serializes to confirm round-trip
//! equality. The snapshot diff is the visual regression check; the
//! `assert_eq!` covers semantic equivalence.

use paigasus_helikon_core::*;

fn roundtrip<T>(value: &T) -> String
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let json = serde_json::to_string_pretty(value).unwrap();
    let parsed: T = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string_pretty(&parsed).unwrap();
    assert_eq!(json, json2, "round-trip mismatch");
    json
}

// --- Item ---

#[test]
fn item_user_message_roundtrip() {
    let item = Item::UserMessage {
        content: vec![ContentPart::Text { text: "hello".into() }],
    };
    insta::assert_snapshot!(roundtrip(&item));
}

#[test]
fn item_assistant_message_roundtrip() {
    let item = Item::AssistantMessage {
        content: vec![
            ContentPart::Text { text: "let me check".into() },
            ContentPart::Reasoning { text: "the user asked X".into() },
        ],
        agent: Some("triage".into()),
    };
    insta::assert_snapshot!(roundtrip(&item));
}

#[test]
fn item_system_roundtrip() {
    let item = Item::System {
        content: vec![ContentPart::Text { text: "you are a helpful assistant".into() }],
    };
    insta::assert_snapshot!(roundtrip(&item));
}

#[test]
fn item_tool_call_roundtrip() {
    let item = Item::ToolCall {
        call_id: "call_abc".into(),
        name: "calculator".into(),
        args: serde_json::json!({ "expr": "1+1" }),
    };
    insta::assert_snapshot!(roundtrip(&item));
}

#[test]
fn item_tool_result_roundtrip() {
    let item = Item::ToolResult {
        call_id: "call_abc".into(),
        content: vec![ContentPart::Text { text: "2".into() }],
    };
    insta::assert_snapshot!(roundtrip(&item));
}

// --- ContentPart ---

#[test]
fn content_part_text_roundtrip() {
    let part = ContentPart::Text { text: "hi".into() };
    insta::assert_snapshot!(roundtrip(&part));
}

#[test]
fn content_part_image_roundtrip() {
    let part = ContentPart::Image {
        source: MediaSource::Url { url: "https://example.com/cat.png".into() },
    };
    insta::assert_snapshot!(roundtrip(&part));
}

#[test]
fn content_part_audio_roundtrip() {
    let part = ContentPart::Audio {
        source: MediaSource::Base64 {
            mime_type: "audio/wav".into(),
            data: "UklGRg==".into(),
        },
    };
    insta::assert_snapshot!(roundtrip(&part));
}

#[test]
fn content_part_tool_use_roundtrip() {
    let part = ContentPart::ToolUse {
        call_id: "call_xyz".into(),
        name: "search".into(),
        args: serde_json::json!({ "q": "rust" }),
    };
    insta::assert_snapshot!(roundtrip(&part));
}

#[test]
fn content_part_tool_result_roundtrip() {
    let part = ContentPart::ToolResult {
        call_id: "call_xyz".into(),
        content: vec![ContentPart::Text { text: "result".into() }],
    };
    insta::assert_snapshot!(roundtrip(&part));
}

#[test]
fn content_part_reasoning_roundtrip() {
    let part = ContentPart::Reasoning { text: "considering...".into() };
    insta::assert_snapshot!(roundtrip(&part));
}

// --- MediaSource ---

#[test]
fn media_source_url_roundtrip() {
    let src = MediaSource::Url { url: "https://example.com/img.png".into() };
    insta::assert_snapshot!(roundtrip(&src));
}

#[test]
fn media_source_base64_roundtrip() {
    let src = MediaSource::Base64 {
        mime_type: "image/png".into(),
        data: "iVBORw0KGgo=".into(),
    };
    insta::assert_snapshot!(roundtrip(&src));
}
```

- [ ] **Step 2: Run the test to verify it fails (types don't exist)**

```bash
cargo test -p paigasus-helikon-core --test serde_roundtrip
```

Expected: compile error like `error[E0432]: unresolved import paigasus_helikon_core::Item` (and the same for `ContentPart`, `MediaSource`).

- [ ] **Step 3: Create the `item.rs` module**

Create `crates/paigasus-helikon-core/src/item.rs`:

```rust
//! Canonical wire-format messages and content blocks.
//!
//! [`Item`] is the superset of OpenAI Chat Completions, OpenAI Responses,
//! Anthropic Messages, and Bedrock Converse content shapes. Provider crates
//! serialize the variant native to their wire format and deserialize the
//! variant the provider returns; both round-trip without lossy translation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Canonical wire-format message.
///
/// `ToolCall` and `ToolResult` mirror OpenAI's sibling "tool" role.
/// Anthropic providers emit equivalent [`ContentPart::ToolUse`] and
/// [`ContentPart::ToolResult`] blocks nested inside `AssistantMessage` /
/// `UserMessage` respectively. Both shapes round-trip cleanly through this
/// type.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Item {
    /// A user-authored message.
    UserMessage {
        /// One or more content blocks.
        content: Vec<ContentPart>,
    },
    /// An assistant-authored message.
    AssistantMessage {
        /// One or more content blocks.
        content: Vec<ContentPart>,
        /// Name of the agent that produced this message, when known.
        /// `Option` because the wire format can lose attribution (e.g. a
        /// raw provider response deserialized without context). The
        /// session log keeps `agent: String` because the runner always
        /// knows which agent emitted.
        agent: Option<String>,
    },
    /// A system message.
    System {
        /// One or more content blocks (typically a single `Text` block).
        content: Vec<ContentPart>,
    },
    /// OpenAI-style sibling-role tool call.
    ToolCall {
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        name: String,
        /// JSON arguments.
        args: serde_json::Value,
    },
    /// OpenAI-style "tool" role response.
    ToolResult {
        /// Matching call identifier.
        call_id: String,
        /// One or more content blocks (Anthropic permits text + image inside
        /// a tool result).
        content: Vec<ContentPart>,
    },
}

/// One content block inside an [`Item`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContentPart {
    /// Plain text.
    Text {
        /// The text payload.
        text: String,
    },
    /// An image, by URL or inline base64.
    Image {
        /// Where the image bytes come from.
        source: MediaSource,
    },
    /// Audio, by URL or inline base64.
    Audio {
        /// Where the audio bytes come from.
        source: MediaSource,
    },
    /// Anthropic-style tool_use block nested inside an `AssistantMessage`.
    /// Equivalent to a top-level [`Item::ToolCall`].
    ToolUse {
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        name: String,
        /// JSON arguments.
        args: serde_json::Value,
    },
    /// Anthropic-style tool_result block nested inside a `UserMessage`.
    /// Equivalent to a top-level [`Item::ToolResult`]. The inner content is
    /// itself a `Vec<ContentPart>` because Anthropic permits text + image
    /// blocks inside a tool_result.
    ToolResult {
        /// Matching call identifier.
        call_id: String,
        /// Content blocks comprising the tool's output.
        content: Vec<ContentPart>,
    },
    /// Provider-emitted reasoning trace (e.g. Anthropic extended thinking,
    /// OpenAI reasoning summaries).
    Reasoning {
        /// The reasoning text payload.
        text: String,
    },
}

/// Source of a multimedia content block.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum MediaSource {
    /// Remote URL.
    Url {
        /// Absolute URL of the media resource.
        url: String,
    },
    /// Inline base64-encoded bytes.
    Base64 {
        /// IANA media type (e.g. `image/png`, `audio/wav`).
        mime_type: String,
        /// Base64-encoded payload.
        data: String,
    },
}
```

- [ ] **Step 4: Wire `item` into `lib.rs`**

Edit `crates/paigasus-helikon-core/src/lib.rs`. Add `pub mod item;` in alphabetical position (between `hook` and `model`) and `pub use item::*;` in matching position:

```rust
pub mod agent;
pub mod context;
pub mod guardrail;
pub mod hook;
pub mod item;
pub mod model;
pub mod runner;
pub mod session;
pub mod tool;

pub use agent::*;
pub use context::*;
pub use guardrail::*;
pub use hook::*;
pub use item::*;
pub use model::*;
pub use runner::*;
pub use session::*;
pub use tool::*;
```

- [ ] **Step 5: Run the test — types now exist, but snapshots don't**

```bash
cargo test -p paigasus-helikon-core --test serde_roundtrip
```

Expected: 13 tests fail with `[new] item_user_message_roundtrip` (or similar `[new]` markers) because `insta` cannot find existing snapshot files.

- [ ] **Step 6: Auto-accept the new snapshots**

```bash
INSTA_UPDATE=auto cargo test -p paigasus-helikon-core --test serde_roundtrip
```

Expected: all 13 tests pass; `tests/snapshots/serde_roundtrip__*.snap` files are written.

- [ ] **Step 7: Inspect the generated snapshots**

```bash
ls crates/paigasus-helikon-core/tests/snapshots/
```

Expected: 13 `.snap` files, one per test. Open one or two (e.g., `serde_roundtrip__item_user_message_roundtrip.snap`) and confirm the JSON shape looks right — the snapshot file contains an `insta` metadata header followed by the prettified JSON like:

```
---
source: crates/paigasus-helikon-core/tests/serde_roundtrip.rs
expression: roundtrip(&item)
---
{
  "type": "user_message",
  "content": [
    {
      "type": "text",
      "text": "hello"
    }
  ]
}
```

- [ ] **Step 8: Re-run the tests without auto-update to confirm round-trip stability**

```bash
cargo test -p paigasus-helikon-core --test serde_roundtrip
```

Expected: 13 tests pass.

- [ ] **Step 9: Run the full crate test suite**

```bash
cargo test -p paigasus-helikon-core --all-features
```

Expected: all tests pass (the new 13 plus the existing `object_safety` test from SMA-312).

- [ ] **Step 10: Verify lints and docs are clean**

```bash
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --all-features --no-deps
```

Expected: both exit 0.

- [ ] **Step 11: Commit**

```bash
git add crates/paigasus-helikon-core/src/item.rs \
        crates/paigasus-helikon-core/src/lib.rs \
        crates/paigasus-helikon-core/tests/serde_roundtrip.rs \
        crates/paigasus-helikon-core/tests/snapshots/
git commit -m "$(cat <<'EOF'
feat(core): SMA-313 add Item, ContentPart, MediaSource wire-format types

Canonical wire-format ADT for messages exchanged across the trait
surface. `Item::ToolCall` / `Item::ToolResult` (OpenAI sibling-role
style) and `ContentPart::ToolUse` / `ContentPart::ToolResult`
(Anthropic content-block style) coexist by design so provider crates
serialize natively without lossy translation. 13 round-trip tests
lock the serde shape.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Add `TokenUsage` to `runner.rs`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs`

- [ ] **Step 1: Add the `TokenUsage` struct beneath the existing `RunResultStreaming` definition**

Open `crates/paigasus-helikon-core/src/runner.rs`. The file currently ends with the `RunError` enum. Insert the following block immediately **before** the `RunError` definition (or anywhere in the file that keeps related types together):

```rust
/// Token usage aggregated across all turns of a run.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq,
    serde::Serialize, serde::Deserialize,
)]
#[non_exhaustive]
pub struct TokenUsage {
    /// Prompt tokens billed for this run.
    pub input_tokens: u64,
    /// Completion tokens billed for this run.
    pub output_tokens: u64,
    /// Tokens served from prompt cache (OpenAI prompt-caching, Anthropic
    /// prompt-caching). Counted as `input_tokens` by the provider; this
    /// field is informational.
    pub cached_input_tokens: u64,
    /// Reasoning tokens billed (OpenAI o-series, Anthropic extended
    /// thinking).
    pub reasoning_tokens: u64,
    /// Total tokens billed across all categories.
    pub total_tokens: u64,
}

impl TokenUsage {
    /// Add another usage record (per-turn aggregation across a run).
    pub fn add(&mut self, other: TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.total_tokens += other.total_tokens;
    }
}
```

- [ ] **Step 2: Verify the crate compiles**

```bash
cargo check -p paigasus-helikon-core
```

Expected: exits 0.

- [ ] **Step 3: Verify lints and docs are clean**

```bash
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --all-features --no-deps
```

Expected: both exit 0.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-core/src/runner.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-313 add TokenUsage carrier

Aggregates input, output, cached-input, and reasoning tokens with a
total across all turns of a run. Used by RunResult.usage and
AgentEvent::RunCompleted.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Generalize `RunResult` over the structured-output type

**Files:**
- Modify: `crates/paigasus-helikon-core/src/runner.rs`

- [ ] **Step 1: Replace the unit-like `RunResult` with a generic struct**

In `crates/paigasus-helikon-core/src/runner.rs`, find the current `RunResult` block:

```rust
/// The aggregated outcome of a non-streaming [`Runner::run`]. Field shape
/// (final response, trajectory, token counts) lands with the runner
/// ticket.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct RunResult {}
```

Replace it with the generic version. Also update the imports at the top of the file (currently `use crate::{Agent, AgentError, AgentInput, RunContext};` — add `AgentEvent` to the import list since the new `RunResult` uses it):

```rust
use crate::{Agent, AgentError, AgentEvent, AgentInput, RunContext};
```

And replace the `RunResult` block with:

```rust
/// The aggregated outcome of a non-streaming [`Runner::run`].
///
/// Generic over the structured-output type. The default `T = String`
/// makes the common case ergonomic; structured-output callers build
/// `RunResult<MyStruct>` via [`RunResult::parse_final`].
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct RunResult<T = String> {
    /// The model's final assistant output, deserialized into `T`. For the
    /// default `T = String` this is the literal text.
    pub final_output: T,
    /// Every [`AgentEvent`] emitted during the run, in order.
    pub events: Vec<AgentEvent>,
    /// Aggregated token usage across every turn of the run.
    pub usage: TokenUsage,
}

impl RunResult<String> {
    /// Deserialize `final_output` into `T`, producing a typed
    /// [`RunResult`].
    ///
    /// The `T: JsonSchema` bound is the marker that the caller has
    /// configured structured output upstream — without it, `parse_final`
    /// is just a JSON parse over unstructured text.
    pub fn parse_final<T>(self) -> Result<RunResult<T>, serde_json::Error>
    where
        T: serde::de::DeserializeOwned + schemars::JsonSchema,
    {
        let final_output = serde_json::from_str::<T>(&self.final_output)?;
        Ok(RunResult {
            final_output,
            events: self.events,
            usage: self.usage,
        })
    }
}
```

- [ ] **Step 2: Verify the existing rustdoc example still compiles**

The current `Runner` trait rustdoc example contains `Ok(RunResult::default())`. With the new generic `RunResult<T = String>`, `RunResult::default()` resolves to `RunResult::<String>::default()` (and `String: Default`), so the example still compiles unchanged. Verify:

```bash
cargo test -p paigasus-helikon-core --doc
```

Expected: exits 0; doctests pass.

- [ ] **Step 3: Verify the crate compiles and existing tests pass**

```bash
cargo test -p paigasus-helikon-core --all-features
```

Expected: exits 0. The `tests/object_safety.rs` test from SMA-312 keeps passing — `Runner::run`'s signature is unchanged.

- [ ] **Step 4: Verify lints and docs are clean**

```bash
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --all-features --no-deps
```

Expected: both exit 0.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/runner.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-313 generalize RunResult over structured output type

RunResult<T = String> with fields final_output, events, usage.
Object-safety on Runner::run preserved via the default type parameter
(return type continues to mean RunResult<String> at the trait site).
parse_final::<T>() on RunResult<String> is the structured-output path.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Derive `Serialize` and `Deserialize` on `GuardrailKind`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/guardrail.rs`

`AgentEvent::GuardrailTriggered` (added in Task 6) carries a `GuardrailKind` field. For the event to round-trip via serde, `GuardrailKind` needs the derives.

- [ ] **Step 1: Add the derives**

Open `crates/paigasus-helikon-core/src/guardrail.rs`. Find the `GuardrailKind` enum:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GuardrailKind {
```

Add `Serialize` and `Deserialize` to the derive list, and add the `#[serde(tag = "type", rename_all = "snake_case")]` attribute to match the workspace's wire-format convention:

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum GuardrailKind {
```

- [ ] **Step 2: Verify the crate compiles**

```bash
cargo check -p paigasus-helikon-core
```

Expected: exits 0.

- [ ] **Step 3: Verify the existing test suite passes**

```bash
cargo test -p paigasus-helikon-core --all-features
```

Expected: exits 0.

- [ ] **Step 4: Verify lints are clean**

```bash
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
```

Expected: exits 0.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/src/guardrail.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-313 derive serde on GuardrailKind

Required so AgentEvent::GuardrailTriggered (lands in the next commit)
can round-trip through JSON. Tagged with #[serde(tag = "type")] to
match the workspace wire-format convention.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Expand `AgentEvent` to the full 14-variant ADT

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs`
- Modify: `crates/paigasus-helikon-core/tests/serde_roundtrip.rs`

- [ ] **Step 1: Append 14 round-trip tests to `tests/serde_roundtrip.rs`**

Open `crates/paigasus-helikon-core/tests/serde_roundtrip.rs`. Append a new section at the end of the file, after the `MediaSource` tests:

```rust
// --- AgentEvent ---

#[test]
fn agent_event_run_started_roundtrip() {
    let ev = AgentEvent::RunStarted { agent: "triage".into() };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_turn_started_roundtrip() {
    let ev = AgentEvent::TurnStarted { turn: 1 };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_token_delta_roundtrip() {
    let ev = AgentEvent::TokenDelta { text: "hel".into() };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_reasoning_delta_roundtrip() {
    let ev = AgentEvent::ReasoningDelta { text: "let me think".into() };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_tool_call_delta_roundtrip() {
    let ev = AgentEvent::ToolCallDelta {
        call_id: "call_1".into(),
        name: Some("calc".into()),
        args_delta: "{\"x\":".into(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_message_output_roundtrip() {
    let ev = AgentEvent::MessageOutput {
        item: Item::AssistantMessage {
            content: vec![ContentPart::Text { text: "hello".into() }],
            agent: Some("triage".into()),
        },
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_tool_call_item_roundtrip() {
    let ev = AgentEvent::ToolCallItem {
        item: Item::ToolCall {
            call_id: "call_1".into(),
            name: "calc".into(),
            args: serde_json::json!({ "expr": "1+1" }),
        },
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_tool_output_item_roundtrip() {
    let ev = AgentEvent::ToolOutputItem {
        item: Item::ToolResult {
            call_id: "call_1".into(),
            content: vec![ContentPart::Text { text: "2".into() }],
        },
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_handoff_item_roundtrip() {
    let ev = AgentEvent::HandoffItem { from: "triage".into(), to: "billing".into() };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_agent_updated_roundtrip() {
    let ev = AgentEvent::AgentUpdated { agent: "billing".into() };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_guardrail_triggered_roundtrip() {
    let ev = AgentEvent::GuardrailTriggered {
        kind: GuardrailKind::InputPolicy,
        info: serde_json::json!({ "score": 0.92 }),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_approval_requested_roundtrip() {
    let ev = AgentEvent::ApprovalRequested {
        call_id: "call_1".into(),
        tool: "delete_file".into(),
        args: serde_json::json!({ "path": "/etc/passwd" }),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_run_completed_roundtrip() {
    let ev = AgentEvent::RunCompleted {
        usage: TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cached_input_tokens: 30,
            reasoning_tokens: 10,
            total_tokens: 160,
        },
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn agent_event_run_failed_roundtrip() {
    let ev = AgentEvent::RunFailed { error: "model unavailable".into() };
    insta::assert_snapshot!(roundtrip(&ev));
}
```

- [ ] **Step 2: Run the tests to verify they fail (variants don't exist yet)**

```bash
cargo test -p paigasus-helikon-core --test serde_roundtrip
```

Expected: compile errors like `error[E0599]: no variant or associated item named 'TurnStarted' found for enum 'AgentEvent'` (and similar for every new variant).

- [ ] **Step 3: Replace `AgentEvent` in `agent.rs` with the full 14-variant ADT**

Open `crates/paigasus-helikon-core/src/agent.rs`. Update the imports at the top — the file currently imports `use crate::{GuardrailKind, ModelError, RunContext, SessionError, ToolError};`. Add `Item` and `TokenUsage`:

```rust
use crate::{GuardrailKind, Item, ModelError, RunContext, SessionError, TokenUsage, ToolError};
```

Find the existing `AgentEvent` block (the 4-variant stub) and replace it with:

```rust
/// The unified event stream emitted by an [`Agent`].
///
/// Fourteen variants spanning lifecycle, raw streaming deltas,
/// post-aggregation semantic items, agent transitions, control signals,
/// and terminal outcomes. The semantic-item variants
/// (`MessageOutput`, `ToolCallItem`, `ToolOutputItem`) carry a full
/// [`Item`] — the doc on each variant names the expected inner variant.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentEvent {
    // --- Lifecycle ---
    /// The run has started; the named agent is active.
    RunStarted {
        /// Agent name.
        agent: String,
    },
    /// A new turn (one model invocation plus any tool calls) has begun.
    TurnStarted {
        /// Zero-based turn index within the run.
        turn: u32,
    },

    // --- Raw deltas (for low-latency UIs) ---
    /// An incremental assistant-text chunk.
    TokenDelta {
        /// Text fragment.
        text: String,
    },
    /// An incremental reasoning-text chunk.
    ReasoningDelta {
        /// Text fragment.
        text: String,
    },
    /// An incremental tool-call-arguments chunk.
    ToolCallDelta {
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name; `Some` on the first delta only.
        name: Option<String>,
        /// JSON-encoded argument fragment.
        args_delta: String,
    },

    // --- Semantic items (post-aggregation; carry Item) ---
    /// A complete assistant message produced by the model. The inner
    /// [`Item`] is expected to be [`Item::AssistantMessage`].
    MessageOutput {
        /// The complete message.
        item: Item,
    },
    /// A complete tool call resolved during the turn. The inner [`Item`]
    /// is expected to be [`Item::ToolCall`].
    ToolCallItem {
        /// The complete tool call.
        item: Item,
    },
    /// A complete tool result returned by a tool. The inner [`Item`] is
    /// expected to be [`Item::ToolResult`].
    ToolOutputItem {
        /// The complete tool result.
        item: Item,
    },
    /// A handoff item recorded in the trajectory.
    HandoffItem {
        /// Outgoing agent name.
        from: String,
        /// Incoming agent name.
        to: String,
    },

    // --- Agent transitions ---
    /// The currently-active agent changed.
    AgentUpdated {
        /// Name of the newly-active agent.
        agent: String,
    },

    // --- Control ---
    /// A guardrail tripwire fired during the run.
    GuardrailTriggered {
        /// Which kind of tripwire fired.
        kind: GuardrailKind,
        /// Free-form context supplied by the guardrail.
        info: serde_json::Value,
    },
    /// The runner is awaiting an approval decision before proceeding.
    ApprovalRequested {
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        tool: String,
        /// JSON arguments the model proposed to call the tool with.
        args: serde_json::Value,
    },

    // --- Terminal ---
    /// The run finished normally.
    RunCompleted {
        /// Aggregated usage across the run.
        usage: TokenUsage,
    },
    /// The run finished with an error.
    RunFailed {
        /// Human-readable error message.
        error: String,
    },
}
```

- [ ] **Step 4: Run the test suite (snapshots will be new)**

```bash
cargo test -p paigasus-helikon-core --test serde_roundtrip
```

Expected: 14 new tests fail with `[new]` markers; the original 13 still pass.

- [ ] **Step 5: Auto-accept the new snapshots**

```bash
INSTA_UPDATE=auto cargo test -p paigasus-helikon-core --test serde_roundtrip
```

Expected: all 27 tests pass; 14 new `.snap` files appear under `tests/snapshots/`.

- [ ] **Step 6: Inspect one or two of the new snapshots**

```bash
ls crates/paigasus-helikon-core/tests/snapshots/ | grep agent_event
```

Open `serde_roundtrip__agent_event_message_output_roundtrip.snap` and confirm the JSON nests cleanly:

```json
{
  "type": "message_output",
  "item": {
    "type": "assistant_message",
    "content": [
      {
        "type": "text",
        "text": "hello"
      }
    ],
    "agent": "triage"
  }
}
```

- [ ] **Step 7: Re-run tests without auto-update**

```bash
cargo test -p paigasus-helikon-core --test serde_roundtrip
```

Expected: 27 tests pass.

- [ ] **Step 8: Run the full crate test suite**

```bash
cargo test -p paigasus-helikon-core --all-features
```

Expected: exits 0. The `tests/object_safety.rs` test continues to pass — `AgentEvent` is `#[non_exhaustive]` and the trait signature using it has not changed.

- [ ] **Step 9: Verify lints and docs are clean**

```bash
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --all-features --no-deps
```

Expected: both exit 0.

- [ ] **Step 10: Commit**

```bash
git add crates/paigasus-helikon-core/src/agent.rs \
        crates/paigasus-helikon-core/tests/serde_roundtrip.rs \
        crates/paigasus-helikon-core/tests/snapshots/
git commit -m "$(cat <<'EOF'
feat(core): SMA-313 expand AgentEvent to full 14-variant ADT

Replaces the 4-variant stub with the full event ADT spanning
lifecycle, raw streaming deltas, post-aggregation semantic items,
agent transitions, control signals, and terminal outcomes. Semantic
items (MessageOutput, ToolCallItem, ToolOutputItem) carry Item
directly to reuse the canonical content carrier.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Add `TracerHandle` and `HookRegistry<Ctx>` to `context.rs`

**Files:**
- Modify: `crates/paigasus-helikon-core/src/context.rs`

Both types need to exist before Task 8 fills in `RunContext` (which references them).

- [ ] **Step 1: Update the imports at the top of `context.rs`**

Open `crates/paigasus-helikon-core/src/context.rs`. The current top of the file imports `use std::marker::PhantomData;` and re-exports `CancellationToken`. Replace the top of the file (everything before the existing `RunContext` definition) with:

```rust
//! Run-scoped context types.
//!
//! [`RunContext`] carries user data, the session handle, the hook
//! registry, the tracer handle, and the cancellation token across the
//! agent loop. [`HookRegistry`] and [`TracerHandle`] are supporting
//! carriers whose full surface lands with the agent-loop and
//! observability tickets respectively.

use std::sync::Arc;

use crate::Hook;
```

(The `PhantomData` import is removed because Task 8 will replace the `PhantomData`-only `RunContext` with one carrying real fields. Don't remove the `pub use tokio_util::sync::CancellationToken;` line — keep it; we'll add it back at the bottom in Task 8.)

Actually — to avoid breaking the file mid-task, leave the existing `RunContext` and `CancellationToken` re-export in place for now. Just **add** the new `use std::sync::Arc;` and `use crate::Hook;` lines below the existing `use std::marker::PhantomData;`.

- [ ] **Step 2: Append `HookRegistry<Ctx>` and `TracerHandle` to `context.rs`**

At the bottom of `crates/paigasus-helikon-core/src/context.rs`, after the existing `pub use tokio_util::sync::CancellationToken;` line, append:

```rust

/// Registry of hooks active for one run.
///
/// Today the surface is intentionally minimal — just `new`, `push`,
/// `iter`, and `is_empty`. The agent-loop ticket grows this when it
/// needs per-event filtering.
pub struct HookRegistry<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    hooks: Vec<Arc<dyn Hook<Ctx>>>,
}

impl<Ctx> HookRegistry<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct an empty registry.
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    /// Register a hook.
    pub fn push(&mut self, hook: Arc<dyn Hook<Ctx>>) {
        self.hooks.push(hook);
    }

    /// Iterate over registered hooks in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Hook<Ctx>>> {
        self.hooks.iter()
    }

    /// `true` if no hooks are registered.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

impl<Ctx> Default for HookRegistry<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Opaque handle to the per-run tracer.
///
/// Field shape lands with the observability ticket; today this is a
/// unit struct so signatures referring to `TracerHandle` resolve.
// SMA-3xx — gains real fields with the observability ticket.
#[derive(Debug, Clone, Default)]
pub struct TracerHandle {
    _private: (),
}
```

- [ ] **Step 3: Verify the crate compiles**

```bash
cargo check -p paigasus-helikon-core
```

Expected: exits 0.

- [ ] **Step 4: Verify the existing test suite passes**

```bash
cargo test -p paigasus-helikon-core --all-features
```

Expected: exits 0.

- [ ] **Step 5: Verify lints and docs are clean**

```bash
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --all-features --no-deps
```

Expected: both exit 0.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/context.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-313 add TracerHandle and HookRegistry carriers

TracerHandle is a unit-struct placeholder that gains real fields with
the observability ticket. HookRegistry<Ctx> wraps Vec<Arc<dyn Hook<Ctx>>>
with the minimum surface the agent loop needs (new, push, iter,
is_empty, Default).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Fill in `RunContext` and `ToolContext` with real fields

**Files:**
- Modify: `crates/paigasus-helikon-core/src/context.rs`
- Modify: `crates/paigasus-helikon-core/src/tool.rs`

This task replaces the `PhantomData`-only `RunContext` and `ToolContext` with real-field versions.

`tests/object_safety.rs` from SMA-312 needs **no changes**: it already defines `NoopSession`, and its trivial impls only receive `RunContext<()>` / `ToolContext<()>` as method parameters (they don't call `RunContext::new()` or `ToolContext::new()`). After this task, `RunResult::default()` on line 140 still resolves correctly via the `T = String` default. The test must still pass without modification — that's the regression guard.

- [ ] **Step 1: Replace `RunContext` in `context.rs`**

In `crates/paigasus-helikon-core/src/context.rs`, **remove** the existing `use std::marker::PhantomData;` line (since the new `RunContext` no longer needs it). Update the imports to also bring in `Session`:

```rust
use std::sync::Arc;

use crate::{Hook, Session, ToolContext};
```

Then replace the existing `RunContext<Ctx>` block and its `impl` + `Default` impl with:

```rust
/// Carries the per-run state shared across the agent loop, tools,
/// guardrails, and hooks.
///
/// `RunContext` does **not** implement `Default` — a context without a
/// session handle is meaningless. Construct via [`RunContext::new`].
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use async_trait::async_trait;
/// use paigasus_helikon_core::{
///     CancellationToken, ConversationSnapshot, HookRegistry, RunContext,
///     SequenceId, Session, SessionError, SessionEvent, TracerHandle,
/// };
///
/// struct NoopSession;
/// #[async_trait]
/// impl Session for NoopSession {
///     async fn append(&self, _: &[SessionEvent]) -> Result<(), SessionError> { Ok(()) }
///     async fn events(&self, _: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
///         Ok(Vec::new())
///     }
///     async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
///         Ok(ConversationSnapshot::default())
///     }
/// }
///
/// let _ctx: RunContext<()> = RunContext::new(
///     Arc::new(()),
///     Arc::new(NoopSession),
///     HookRegistry::<()>::new(),
///     TracerHandle::default(),
///     CancellationToken::new(),
/// );
/// ```
pub struct RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    user_ctx: Arc<Ctx>,
    session: Arc<dyn Session>,
    hooks: HookRegistry<Ctx>,
    tracer: TracerHandle,
    cancel: CancellationToken,
}

impl<Ctx> RunContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct a new [`RunContext`].
    pub fn new(
        user_ctx: Arc<Ctx>,
        session: Arc<dyn Session>,
        hooks: HookRegistry<Ctx>,
        tracer: TracerHandle,
        cancel: CancellationToken,
    ) -> Self {
        Self { user_ctx, session, hooks, tracer, cancel }
    }

    /// Borrow the user context.
    pub fn user_ctx(&self) -> &Arc<Ctx> { &self.user_ctx }
    /// Borrow the session handle.
    pub fn session(&self) -> &Arc<dyn Session> { &self.session }
    /// Borrow the hook registry.
    pub fn hooks(&self) -> &HookRegistry<Ctx> { &self.hooks }
    /// Borrow the tracer handle.
    pub fn tracer(&self) -> &TracerHandle { &self.tracer }
    /// Borrow the cancellation token.
    pub fn cancel(&self) -> &CancellationToken { &self.cancel }

    /// Project the narrower [`ToolContext`] from this [`RunContext`].
    ///
    /// Tools receive `user_ctx`, `tracer`, and `cancel` — they do **not**
    /// see the session handle (the runner owns persistence) or the hook
    /// registry (hooks fire around tool invocations, not from inside).
    pub fn to_tool_context(&self) -> ToolContext<Ctx> {
        ToolContext::new(
            Arc::clone(&self.user_ctx),
            self.tracer.clone(),
            self.cancel.clone(),
        )
    }
}
```

The `Default` impl from the previous version is **removed** — that's the intentional source-compat break documented in spec §11.

- [ ] **Step 2: Replace `ToolContext` in `tool.rs`**

Open `crates/paigasus-helikon-core/src/tool.rs`. Replace the imports at the top:

```rust
//! The [`Tool`] trait and its carrier types.
//!
//! Tools are object-safe by design — applications hold heterogeneous
//! registries as `Vec<Arc<dyn Tool<Ctx>>>`.

use std::sync::Arc;

use async_trait::async_trait;

use crate::{CancellationToken, TracerHandle};
```

(The `use std::marker::PhantomData;` line is removed.)

Find the existing `ToolContext<Ctx>` block (including its `impl` and `Default` impl) and replace with:

```rust
/// Narrower view of [`crate::RunContext`] passed to [`Tool::invoke`].
///
/// Deliberately excludes the session handle and hook registry: tools
/// must not bypass the runner's persistence by writing directly to the
/// session log, and hooks fire *around* tool invocations, not from
/// inside them.
pub struct ToolContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    user_ctx: Arc<Ctx>,
    tracer: TracerHandle,
    cancel: CancellationToken,
}

impl<Ctx> ToolContext<Ctx>
where
    Ctx: Send + Sync + 'static,
{
    /// Construct a new [`ToolContext`].
    pub fn new(
        user_ctx: Arc<Ctx>,
        tracer: TracerHandle,
        cancel: CancellationToken,
    ) -> Self {
        Self { user_ctx, tracer, cancel }
    }

    /// Borrow the user context.
    pub fn user_ctx(&self) -> &Arc<Ctx> { &self.user_ctx }
    /// Borrow the tracer handle.
    pub fn tracer(&self) -> &TracerHandle { &self.tracer }
    /// Borrow the cancellation token.
    pub fn cancel(&self) -> &CancellationToken { &self.cancel }
}
```

The previous `Default` impl is removed.

The existing `Tool` trait, rustdoc example, `ToolOutput`, and `ToolError` definitions are unchanged. The doctest on `Tool` (which constructs `_ctx: &ToolContext<()>`) does not call any `ToolContext` constructor itself — it only borrows the type — so it keeps compiling.

- [ ] **Step 3: Run the object-safety test (must pass without modification)**

```bash
cargo test -p paigasus-helikon-core --test object_safety
```

Expected: exits 0. The test passes unchanged because the trait method signatures `Agent::run(_ctx: RunContext<Ctx>, ...)`, `Tool::invoke(_ctx: &ToolContext<Ctx>, ...)`, etc. only *receive* the new context types — they don't construct them. `RunResult::default()` on line 140 of the test still resolves correctly via the `T = String` default parameter.

If this step fails, do **not** modify `object_safety.rs` — instead investigate whether Task 8's `context.rs` or `tool.rs` edit accidentally changed a public-facing API the test depends on.

- [ ] **Step 4: Run the full crate test suite**

```bash
cargo test -p paigasus-helikon-core --all-features
```

Expected: exits 0. The serde round-trip tests from earlier tasks still pass; the object-safety test passes unmodified.

- [ ] **Step 5: Verify lints and docs**

```bash
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --all-features --no-deps
```

Expected: both exit 0. The new rustdoc example on `RunContext` compiles as part of `cargo test --doc`.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/context.rs \
        crates/paigasus-helikon-core/src/tool.rs
git commit -m "$(cat <<'EOF'
feat(core): SMA-313 fill in RunContext and ToolContext with real fields

RunContext now carries user_ctx, session, hooks, tracer, and cancel.
ToolContext is the deliberately narrower view (user_ctx + tracer +
cancel only) constructed via RunContext::to_tool_context — tools
cannot bypass runner-owned session persistence or trigger hooks from
within. Default impls removed on both types because a zero-value
context is a footgun. The SMA-312 object-safety test continues to
pass without modification — its trivial impls only receive contexts
as method parameters; they never construct them.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: Migrate `SessionEvent` to `Vec<ContentPart>` + `ConversationSnapshot.messages` + round-trip tests

**Files:**
- Modify: `crates/paigasus-helikon-core/src/session.rs`
- Modify: `crates/paigasus-helikon-core/tests/serde_roundtrip.rs`

- [ ] **Step 1: Append 6 round-trip tests to `tests/serde_roundtrip.rs`**

Append the following section at the end of `crates/paigasus-helikon-core/tests/serde_roundtrip.rs`:

```rust
// --- SessionEvent ---

#[test]
fn session_event_user_message_roundtrip() {
    let ev = SessionEvent::UserMessage {
        content: vec![ContentPart::Text { text: "hello".into() }],
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn session_event_assistant_message_roundtrip() {
    let ev = SessionEvent::AssistantMessage {
        content: vec![ContentPart::Text { text: "hi back".into() }],
        agent: "triage".into(),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn session_event_tool_called_roundtrip() {
    let ev = SessionEvent::ToolCalled {
        call_id: "call_1".into(),
        name: "calc".into(),
        args: serde_json::json!({ "expr": "1+1" }),
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn session_event_tool_returned_roundtrip() {
    let ev = SessionEvent::ToolReturned {
        call_id: "call_1".into(),
        content: vec![ContentPart::Text { text: "2".into() }],
    };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn session_event_handoff_occurred_roundtrip() {
    let ev = SessionEvent::HandoffOccurred { from: "triage".into(), to: "billing".into() };
    insta::assert_snapshot!(roundtrip(&ev));
}

#[test]
fn session_event_compacted_roundtrip() {
    let ev = SessionEvent::Compacted {
        summary: "user asked for a refund; assistant agreed".into(),
        original_count: 12,
    };
    insta::assert_snapshot!(roundtrip(&ev));
}
```

- [ ] **Step 2: Run the tests to confirm they fail**

```bash
cargo test -p paigasus-helikon-core --test serde_roundtrip
```

Expected: the 4 tests for variants whose shape is changing (`session_event_user_message_roundtrip`, `..._assistant_message_...`, `..._tool_returned_...`) fail with compile errors like `error[E0560]: struct variant SessionEvent::UserMessage has no field named 'content'`. The other 2 (`tool_called`, `handoff_occurred`, `compacted`) compile but show `[new]` markers because their snapshots are new.

- [ ] **Step 3: Migrate `SessionEvent` and `ConversationSnapshot` in `session.rs`**

Open `crates/paigasus-helikon-core/src/session.rs`. Update the imports at the top — the file currently has `use serde::{Deserialize, Serialize};`. Add `use crate::{ContentPart, Item};`:

```rust
use serde::{Deserialize, Serialize};

use crate::{ContentPart, Item};
```

Find the existing `SessionEvent` enum and replace its three migrating variants:

| Variant | Old | New |
|---|---|---|
| `UserMessage` | `{ text: String }` | `{ content: Vec<ContentPart> }` |
| `AssistantMessage` | `{ text: String, agent: String }` | `{ content: Vec<ContentPart>, agent: String }` |
| `ToolReturned` | `{ call_id: String, output: serde_json::Value }` | `{ call_id: String, content: Vec<ContentPart> }` |

The resulting `SessionEvent` block:

```rust
/// One entry in the conversation event log.
///
/// `UserMessage` / `AssistantMessage` / `ToolReturned` carry
/// `Vec<ContentPart>` directly (not `Item`) because the SessionEvent
/// variant *is* the role — wrapping `Item::UserMessage` inside
/// `SessionEvent::UserMessage` would double-tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionEvent {
    /// A user-authored message.
    UserMessage {
        /// Content blocks of the message.
        content: Vec<ContentPart>,
    },
    /// An assistant-authored message attributed to a named agent.
    AssistantMessage {
        /// Content blocks of the message.
        content: Vec<ContentPart>,
        /// Name of the emitting [`crate::Agent`]. `String` (not `Option`)
        /// because the runner always knows which agent emitted when
        /// appending to the log.
        agent: String,
    },
    /// The runner invoked a tool.
    ToolCalled {
        /// Provider-assigned call identifier.
        call_id: String,
        /// Tool name.
        name: String,
        /// JSON arguments.
        args: serde_json::Value,
    },
    /// The tool returned.
    ToolReturned {
        /// Matching call identifier.
        call_id: String,
        /// Content blocks of the tool's output (Anthropic permits
        /// text + image inside a tool result).
        content: Vec<ContentPart>,
    },
    /// Control transferred from one agent to another.
    HandoffOccurred {
        /// Outgoing agent name.
        from: String,
        /// Incoming agent name.
        to: String,
    },
    /// Older events were compacted into a summary.
    Compacted {
        /// LLM-produced summary.
        summary: String,
        /// Number of events the summary replaces. `u64` (not `usize`)
        /// because the value is serialized into the persisted log — a
        /// 32-bit consumer must read what a 64-bit producer wrote.
        original_count: u64,
    },
}
```

Then update `ConversationSnapshot` to add a `messages: Vec<Item>` field:

```rust
/// A computed projection of a [`Session`]'s log into a single
/// conversation state. The `messages` field is the canonical view a
/// session emits.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ConversationSnapshot {
    /// Canonical message list, in conversational order.
    pub messages: Vec<Item>,
}
```

- [ ] **Step 4: Update the rustdoc example on `Session`**

The existing `Session` doctest constructs `ConversationSnapshot::default()` — that still works because the new shape with `messages: Vec<Item>` derives `Default` (`Vec` is `Default`). No edit needed.

- [ ] **Step 5: Run the tests — variants now exist, but snapshots don't**

```bash
cargo test -p paigasus-helikon-core --test serde_roundtrip
```

Expected: 6 new tests fail with `[new]` markers; the existing 27 pass.

- [ ] **Step 6: Auto-accept the new snapshots**

```bash
INSTA_UPDATE=auto cargo test -p paigasus-helikon-core --test serde_roundtrip
```

Expected: all 33 tests pass; 6 new `.snap` files appear.

- [ ] **Step 7: Re-run without auto-update to confirm round-trip stability**

```bash
cargo test -p paigasus-helikon-core --test serde_roundtrip
```

Expected: all 33 tests pass.

- [ ] **Step 8: Run the full crate test suite**

```bash
cargo test -p paigasus-helikon-core --all-features
```

Expected: exits 0.

- [ ] **Step 9: Verify lints and docs are clean**

```bash
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --all-features --no-deps
```

Expected: both exit 0.

- [ ] **Step 10: Commit**

```bash
git add crates/paigasus-helikon-core/src/session.rs \
        crates/paigasus-helikon-core/tests/serde_roundtrip.rs \
        crates/paigasus-helikon-core/tests/snapshots/
git commit -m "$(cat <<'EOF'
feat(core): SMA-313 migrate SessionEvent and ConversationSnapshot to Item content

UserMessage / AssistantMessage / ToolReturned variants now carry
Vec<ContentPart> directly instead of String / serde_json::Value, so
the wire format is end-to-end coherent. ConversationSnapshot gains a
messages: Vec<Item> projection. Breaking change to a freshly-shipped
enum is acceptable because SMA-312 has no downstream consumers yet.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: Lock AC #2 — `RunResult<MyStruct>` compile test

**Files:**
- Create: `crates/paigasus-helikon-core/tests/compile_run_result_typed.rs`

- [ ] **Step 1: Create the compile-test file**

Create `crates/paigasus-helikon-core/tests/compile_run_result_typed.rs`:

```rust
//! Locks AC #2 — `RunResult<MyStruct>` compiles when
//! `MyStruct: DeserializeOwned + JsonSchema`, and round-trips via
//! `RunResult::<String>::parse_final`.

use paigasus_helikon_core::{RunResult, TokenUsage};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, PartialEq, Deserialize, JsonSchema)]
struct Answer {
    answer: u32,
}

#[test]
fn run_result_default_t_is_string() {
    // RunResult with no type parameter must resolve to RunResult<String>.
    let r: RunResult = RunResult {
        final_output: "hi".into(),
        events: Vec::new(),
        usage: TokenUsage::default(),
    };
    assert_eq!(r.final_output, "hi");
}

#[test]
fn run_result_with_user_struct_compiles() {
    // RunResult<MyStruct> with MyStruct: DeserializeOwned + JsonSchema.
    let r: RunResult<Answer> = RunResult {
        final_output: Answer { answer: 42 },
        events: Vec::new(),
        usage: TokenUsage::default(),
    };
    assert_eq!(r.final_output.answer, 42);
}

#[test]
fn parse_final_deserializes_json_output() {
    let from_runner = RunResult::<String> {
        final_output: r#"{"answer": 42}"#.into(),
        events: Vec::new(),
        usage: TokenUsage::default(),
    };
    let typed: RunResult<Answer> = from_runner.parse_final::<Answer>().unwrap();
    assert_eq!(typed.final_output, Answer { answer: 42 });
}

#[test]
fn parse_final_propagates_serde_error_on_bad_json() {
    let from_runner = RunResult::<String> {
        final_output: "not json".into(),
        events: Vec::new(),
        usage: TokenUsage::default(),
    };
    let err = from_runner.parse_final::<Answer>().unwrap_err();
    // The error came from serde_json; we just verify we got one.
    assert!(err.to_string().contains("expected"));
}
```

- [ ] **Step 2: Run the test**

```bash
cargo test -p paigasus-helikon-core --test compile_run_result_typed
```

Expected: all 4 tests pass.

- [ ] **Step 3: Run the full crate test suite**

```bash
cargo test -p paigasus-helikon-core --all-features
```

Expected: exits 0.

- [ ] **Step 4: Verify lints and docs are clean**

```bash
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-core --all-features --no-deps
```

Expected: both exit 0.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-core/tests/compile_run_result_typed.rs
git commit -m "$(cat <<'EOF'
test(core): SMA-313 add RunResult<T> compile test

Locks AC #2: RunResult<MyStruct> compiles when
MyStruct: DeserializeOwned + JsonSchema. Also exercises
parse_final::<T>() on RunResult<String> and confirms serde errors
propagate on malformed JSON.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Full CI verification

Run every gate that `.github/workflows/ci.yml` runs, plus the MSRV verifier, locally. Each command must exit 0 before the PR is ready for review.

**Files:** none (verification only)

- [ ] **Step 1: Format check**

```bash
cargo fmt --all -- --check
```

Expected: exits 0 (no formatting diffs).

- [ ] **Step 2: Workspace clippy (all features, all targets)**

```bash
cargo clippy --workspace --all-features --all-targets -- -D warnings
```

Expected: exits 0.

- [ ] **Step 3: Workspace tests (all features)**

```bash
cargo test --workspace --all-features
```

Expected: exits 0. The new `paigasus-helikon-core` test files all pass:
- `tests/serde_roundtrip.rs` — 33 tests
- `tests/compile_run_result_typed.rs` — 4 tests
- `tests/object_safety.rs` — 1 test (unchanged AC lock from SMA-312)
- Plus any doctests (`Session`, `RunContext` example).

- [ ] **Step 4: Workspace docs (warnings as errors)**

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: exits 0.

- [ ] **Step 5: Doc coverage threshold**

```bash
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 \
  bash scripts/check-doc-coverage.sh
```

Expected: exits 0. The new public items in `item.rs`, the expanded `AgentEvent` variants, and the filled-in `RunContext` / `ToolContext` accessors all carry `///` docs, keeping the per-crate doc-coverage above 80%.

If this command fails because the nightly toolchain isn't installed locally:

```bash
rustup toolchain install nightly-2026-05-01
```

Then retry the doc-coverage check.

- [ ] **Step 6: MSRV verification**

```bash
cargo msrv --path crates/paigasus-helikon-core verify
```

Expected: exits 0; the crate still builds on Rust 1.75. `schemars 1` and `insta 1` both have MSRV ≤ 1.75.

If `cargo-msrv` isn't installed:

```bash
cargo install cargo-msrv --locked
```

Then retry the verification.

- [ ] **Step 7: Final sanity sweep on the branch state**

```bash
git status
# Expected: working tree clean

git log --oneline main..HEAD
# Expected (most recent first; 12 commits total):
# <hash> test(core): SMA-313 add RunResult<T> compile test
# <hash> feat(core): SMA-313 migrate SessionEvent and ConversationSnapshot to Item content
# <hash> feat(core): SMA-313 fill in RunContext and ToolContext with real fields
# <hash> feat(core): SMA-313 add TracerHandle and HookRegistry carriers
# <hash> feat(core): SMA-313 expand AgentEvent to full 14-variant ADT
# <hash> feat(core): SMA-313 derive serde on GuardrailKind
# <hash> feat(core): SMA-313 generalize RunResult over structured output type
# <hash> feat(core): SMA-313 add TokenUsage carrier
# <hash> feat(core): SMA-313 add Item, ContentPart, MediaSource wire-format types
# <hash> chore(deps): SMA-313 wire schemars and insta into core crate
# <hash> docs(spec): SMA-313 correct schemars pin reference
# <hash> docs(spec): SMA-313 add design for concrete shared types
```

(Note: 10 implementation commits + 2 spec commits = 12 commits on the feature branch above `main`.)

- [ ] **Step 8: Push and open the PR**

```bash
git push -u origin feature/sma-313-concrete-shared-types-item-agentevent-runcontext-runresult

gh pr create --title "feat(core): SMA-313 concrete shared types (Item, AgentEvent, RunContext, RunResult, ToolContext)" --body "$(cat <<'EOF'
## Summary

- Fills in the data plane that the seven SMA-312 traits exchange.
- New `Item` / `ContentPart` / `MediaSource` wire format; full 14-variant `AgentEvent`; `RunResult<T = String>` with `parse_final`; real fields on `RunContext` / `ToolContext`; `TokenUsage`, `HookRegistry<Ctx>`, `TracerHandle`.
- Migrates `SessionEvent::{UserMessage, AssistantMessage, ToolReturned}` to carry `Vec<ContentPart>`; `ConversationSnapshot.messages: Vec<Item>`.
- Object-safety on `Runner::run` preserved via the default type parameter — its signature is unchanged from SMA-312.

Closes SMA-313.

## Test plan

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-features --all-targets -- -D warnings`
- [ ] `cargo test --workspace --all-features` (33 serde round-trip tests + 4 RunResult<T> compile tests + object-safety test from SMA-312)
- [ ] `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
- [ ] `DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh`
- [ ] `cargo msrv --path crates/paigasus-helikon-core verify`

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: PR is created and the URL is printed. CI runs the same six gates that this task ran locally, plus the matrix variants (`test (ubuntu-latest, 1.75)`, `test (macos-latest, ...)`, `test (windows-latest, ...)`), plus `audit`, `deny`, `commits`, and `pr-title`. The required-status checks gate merge; matrix variants run as informational signals.

---

## Notes for the implementer

- **TDD discipline**: For type-shape work, "the failing test" is `cargo check` (or the round-trip test attempting to use a not-yet-defined variant). Each task's Step 1-2 establishes the failure; subsequent steps drive it to green.
- **Snapshot regeneration**: Always run with `INSTA_UPDATE=auto` exactly once after adding new tests, then re-run without it to confirm round-trip stability. Don't commit snapshots without running the bare `cargo test ...` command at least once afterwards.
- **Commit hooks**: The repo has a local commit-msg hook (SMA-335) that enforces Conventional Commits with a scope allowlist. Every commit message above follows `<type>(<scope>): SMA-313 <message>`. If the hook rejects a `test(core):` prefix, fall back to `feat(core):` or `chore(core):` — the test additions are part of the AC lock either way.
- **Object-safety regression risk**: The `Runner::run` trait signature is unchanged. If a future edit accidentally adds a method-level generic to `Runner`, `tests/object_safety.rs` catches it on the next CI run.
- **Doc-coverage sensitivity**: Every new public field needs a `///` line — there are ~40 new public items across this ticket. The doc-coverage gate (80%) is forgiving of the per-crate ratio, but missing-docs warnings are upgraded to errors in CI via `RUSTDOCFLAGS="-D warnings"`. Run the docs check locally after each task.
- **No `feature/` rebasing**: Don't rebase the feature branch onto `main` mid-ticket unless `main` has moved underneath you. The branch protection ruleset (SMA-309) doesn't require linear history; squash-merge collapses the implementation commits into one at PR-merge time.
