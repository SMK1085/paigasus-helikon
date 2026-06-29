# SMA-330 Production Session Backends — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add production `Session` backends — `PostgresSession`, `RedisSession`, and a generic `CompactingSession<S>` — plus a shared conformance harness, all implementing the existing `paigasus_helikon_core::Session` trait.

**Architecture:** `CompactingSession`, `TokenCounter`, and three new `SessionEvent` accessors are **additive** to `paigasus-helikon-core` (alongside `MemorySession`/`project`). Postgres and Redis are two **new published** crates mirroring `paigasus-helikon-sessions-sqlite`. A shared conformance suite lives in a **new unpublished** crate `paigasus-helikon-sessions-testkit`, consumed as a path-only dev-dependency. Delivery is **two sequential PRs** (PR-1: core + testkit + sqlite; PR-2: postgres + redis + facade + CI), with PR-2 branched only after PR-1's core publishes to crates.io.

**Tech Stack:** Rust (workspace, edition 2021, MSRV 1.94), `async-trait`, `sqlx` 0.9 (Postgres), `redis` (tokio + streams + Lua), `futures-util`, `tokio`, `serde_json`, `jiff`.

**Design spec (read first, referenced throughout):** `docs/superpowers/specs/2026-06-28-sma-330-production-session-backends-design.md`. Section references below are to that spec.

## Global Constraints

- **MSRV `1.94`** (`[workspace.package].rust-version`). If a new dep demands higher, raise the floor — never downgrade the dep. (spec §11)
- **Workspace inheritance mandatory:** new crates set only `name`, `description`, crate-specific bits; everything else inherits via `.workspace = true`. Copy the `[lints] workspace = true` block. (CLAUDE.md)
- **Every `pub` item needs a `///` doc comment** — the `docs` job runs `RUSTDOCFLAGS=-D warnings` and `missing_docs = warn`. doc-coverage gate ≥ 80%.
- **Run local fmt + clippy before every commit:** `cargo fmt --all` then `cargo clippy --workspace --all-features --all-targets -- -D warnings` (pre-commit hook is a no-op; pre-push catches it). (memory)
- **Commits:** `<type>(<scope>): SMA-330 <lowercase subject>`. Allowed scopes incl. `core`, `sessions-sqlite`, `facade`, `workflows`, `docs`, `spec`, `plan`. CI/release-plumbing edits use `chore(...)`/`docs(...)`, never `feat`/`fix`. (CLAUDE.md)
- **Never `git add -A`** (`.env`/`.claude` are untracked-but-not-ignored). Stage explicit paths; verify `git show --stat`. (memory)
- **No manual version bumps** in either PR — release-plz handles them (spec §10).
- **The canonical `cargo test --workspace --all-features` is the gate** (not per-crate) for the dual-CryptoProvider class of bug. (memory)
- **`SessionEvent` is `#[non_exhaustive]`**; the new `kind()`/`ts()` `match` has **no `_ =>` arm** (compile-fail on a future variant). (spec §4.3)

---

# PR-1 — core + testkit + sqlite

Branch: `feature/sma-330-sessions-core-compaction-testkit` (rename the current branch at the start — Task 0).

## Task 0: Branch setup

**Files:** none (git only).

- [ ] **Step 1: Rename the working branch**

```bash
git branch -m feature/sma-330-sessions-core-compaction-testkit
git branch --show-current   # expect: feature/sma-330-sessions-core-compaction-testkit
```

The spec + plan commits already on this branch move with the rename.

---

## Task 1: `SessionEvent` accessors (`kind`, `ts`, `ts_nanos_saturating`)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/session.rs` (add an `impl SessionEvent` block near the existing constructors, ~line 204)
- Test: `crates/paigasus-helikon-core/src/session.rs` (extend the in-file `#[cfg(test)]` — add a new `mod accessor_tests`)

**Interfaces:**
- Produces: `SessionEvent::kind(&self) -> &'static str`, `SessionEvent::ts(&self) -> jiff::Timestamp`, `SessionEvent::ts_nanos_saturating(&self) -> i64`. (Consumed by Tasks 6, 9, 10.)

- [ ] **Step 1: Write the failing test**

Add to `crates/paigasus-helikon-core/src/session.rs`:

```rust
#[cfg(test)]
mod accessor_tests {
    use super::*;
    use crate::ContentPart;
    use jiff::Timestamp;

    fn epoch() -> Timestamp {
        Timestamp::from_second(0).unwrap()
    }

    #[test]
    fn kind_matches_serde_tag_for_every_variant() {
        let cases: Vec<(SessionEvent, &str)> = vec![
            (SessionEvent::UserMessage { content: vec![], ts: epoch() }, "user_message"),
            (SessionEvent::AssistantMessage { content: vec![], agent: "a".into(), ts: epoch() }, "assistant_message"),
            (SessionEvent::ToolCalled { call_id: "c".into(), name: "n".into(), args: serde_json::json!({}), ts: epoch() }, "tool_called"),
            (SessionEvent::ToolReturned { call_id: "c".into(), content: vec![], ts: epoch() }, "tool_returned"),
            (SessionEvent::HandoffOccurred { from: "a".into(), to: "b".into(), ts: epoch() }, "handoff_occurred"),
            (SessionEvent::Compacted { summary: "s".into(), original_count: 1, ts: epoch() }, "compacted"),
        ];
        for (ev, tag) in cases {
            assert_eq!(ev.kind(), tag);
            // kind() must equal the serde tag actually written to the wire.
            let json = serde_json::to_value(&ev).unwrap();
            assert_eq!(json["type"], tag);
        }
    }

    #[test]
    fn ts_returns_the_variant_timestamp_and_nanos_saturate() {
        let ev = SessionEvent::UserMessage {
            content: vec![ContentPart::Text { text: "x".into() }],
            ts: Timestamp::from_second(1_700_000_000).unwrap(),
        };
        assert_eq!(ev.ts(), Timestamp::from_second(1_700_000_000).unwrap());
        assert_eq!(ev.ts_nanos_saturating(), 1_700_000_000_000_000_000);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-core accessor_tests`
Expected: FAIL — `no method named kind found`.

- [ ] **Step 3: Implement the accessors**

Add after the existing `impl SessionEvent { … }` constructor block in `session.rs`:

```rust
impl SessionEvent {
    /// The serde tag for this variant (`"user_message"`, `"compacted"`, …).
    /// Matches the `type` field written to the persisted log.
    pub fn kind(&self) -> &'static str {
        // No `_ =>` arm: a new #[non_exhaustive] variant must fail to compile
        // here, in core, rather than silently mis-tagging in a backend.
        match self {
            SessionEvent::UserMessage { .. } => "user_message",
            SessionEvent::AssistantMessage { .. } => "assistant_message",
            SessionEvent::ToolCalled { .. } => "tool_called",
            SessionEvent::ToolReturned { .. } => "tool_returned",
            SessionEvent::HandoffOccurred { .. } => "handoff_occurred",
            SessionEvent::Compacted { .. } => "compacted",
        }
    }

    /// The wall-clock instant this event was recorded.
    pub fn ts(&self) -> Timestamp {
        match self {
            SessionEvent::UserMessage { ts, .. }
            | SessionEvent::AssistantMessage { ts, .. }
            | SessionEvent::ToolCalled { ts, .. }
            | SessionEvent::ToolReturned { ts, .. }
            | SessionEvent::HandoffOccurred { ts, .. }
            | SessionEvent::Compacted { ts, .. } => *ts,
        }
    }

    /// [`Self::ts`] as `i64` nanoseconds since the Unix epoch, saturating to
    /// `i64::MIN`/`i64::MAX` outside ±292 years from 1970. For denormalized
    /// audit-index columns; the canonical timestamp lives in the JSON payload.
    pub fn ts_nanos_saturating(&self) -> i64 {
        let nanos_i128 = self.ts().as_nanosecond();
        let saturated = if nanos_i128 < 0 { i64::MIN } else { i64::MAX };
        i64::try_from(nanos_i128).unwrap_or(saturated)
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p paigasus-helikon-core accessor_tests`
Expected: PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/session.rs
git commit -m "feat(core): SMA-330 add SessionEvent kind/ts/ts_nanos accessors"
```

---

## Task 2: `TokenCounter` trait + `HeuristicTokenCounter`

**Files:**
- Create: `crates/paigasus-helikon-core/src/token_counter.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs` (add `mod token_counter;` + re-exports)
- Test: in `token_counter.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `pub trait TokenCounter: Send + Sync + std::fmt::Debug { fn count(&self, items: &[Item]) -> usize; }` and `pub struct HeuristicTokenCounter;` implementing it. (Consumed by Task 3.)

- [ ] **Step 1: Write the failing test**

Create `crates/paigasus-helikon-core/src/token_counter.rs`:

```rust
//! Token estimation for [`crate::CompactingSession`] threshold decisions.

use crate::Item;

/// Estimates the token cost of a projected conversation, so a
/// [`crate::CompactingSession`] can decide when to summarize.
///
/// Pluggable so callers can supply a model-accurate tokenizer; the default
/// [`HeuristicTokenCounter`] is a cheap, deterministic approximation.
pub trait TokenCounter: Send + Sync + std::fmt::Debug {
    /// Estimate the token count of `items`.
    fn count(&self, items: &[Item]) -> usize;
}

/// Default [`TokenCounter`]: `ceil(total_chars / 4)`, where `total_chars`
/// counts Unicode scalar values across every text-bearing field (see the
/// crate docs and spec §4.1 for the exact enumeration). Deterministic; no deps.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicTokenCounter;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContentPart;

    fn text_item(s: &str) -> Item {
        Item::UserMessage { content: vec![ContentPart::Text { text: s.into() }] }
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(HeuristicTokenCounter.count(&[]), 0);
    }

    #[test]
    fn counts_chars_div_ceil_four() {
        // 5 chars -> ceil(5/4) = 2
        assert_eq!(HeuristicTokenCounter.count(&[text_item("hello")]), 2);
        // multibyte counted as scalar values, not bytes: "héllo" is 5 chars
        assert_eq!(HeuristicTokenCounter.count(&[text_item("héllo")]), 2);
    }

    #[test]
    fn counts_tool_call_args_and_system_summary() {
        let items = vec![
            Item::System { content: vec![ContentPart::Text { text: "summary text".into() }] },
            Item::ToolCall { call_id: "c".into(), name: "calc".into(), args: serde_json::json!({"x": 1}) },
        ];
        // Non-zero: System content + tool name + args JSON all contribute.
        assert!(HeuristicTokenCounter.count(&items) > 0);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-core token_counter`
Expected: FAIL — `count` not implemented for `HeuristicTokenCounter`.

- [ ] **Step 3: Implement `HeuristicTokenCounter::count`**

Append to `token_counter.rs` (above the `#[cfg(test)]`):

```rust
use crate::ContentPart;

impl TokenCounter for HeuristicTokenCounter {
    fn count(&self, items: &[Item]) -> usize {
        let chars: usize = items.iter().map(item_chars).sum();
        chars.div_ceil(4)
    }
}

fn item_chars(item: &Item) -> usize {
    match item {
        Item::UserMessage { content }
        | Item::AssistantMessage { content, .. }
        | Item::System { content }
        | Item::ToolResult { content, .. } => content.iter().map(part_chars).sum(),
        Item::ToolCall { name, args, .. } => {
            name.chars().count() + json_chars(args)
        }
    }
}

fn part_chars(part: &ContentPart) -> usize {
    match part {
        ContentPart::Text { text } | ContentPart::Reasoning { text } => text.chars().count(),
        ContentPart::ToolUse { name, args, .. } => name.chars().count() + json_chars(args),
        ContentPart::ToolResult { content, .. } => content.iter().map(part_chars).sum(),
        // Image/Audio sources are not projected text.
        ContentPart::Image { .. } | ContentPart::Audio { .. } => 0,
    }
}

fn json_chars(v: &serde_json::Value) -> usize {
    // Compact JSON length in chars; deterministic across runs.
    serde_json::to_string(v).map(|s| s.chars().count()).unwrap_or(0)
}
```

- [ ] **Step 4: Wire into `lib.rs`**

In `crates/paigasus-helikon-core/src/lib.rs`, add the module and re-exports next to the session re-exports (match existing style, with `///`-doc'd `pub use` if the file documents re-exports — check neighbors):

```rust
mod token_counter;
pub use token_counter::{HeuristicTokenCounter, TokenCounter};
```

- [ ] **Step 5: Run to verify pass + gates**

```bash
cargo test -p paigasus-helikon-core token_counter
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
```
Expected: PASS, clean.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/token_counter.rs crates/paigasus-helikon-core/src/lib.rs
git commit -m "feat(core): SMA-330 add TokenCounter trait and HeuristicTokenCounter"
```

---

## Task 3: `CompactingSession<S>` + builder

**Files:**
- Create: `crates/paigasus-helikon-core/src/compacting_session.rs`
- Modify: `crates/paigasus-helikon-core/src/lib.rs` (module + re-exports)
- Test: `crates/paigasus-helikon-core/tests/compacting_session.rs`

**Interfaces:**
- Consumes: `Session`, `SessionEvent`, `SessionError`, `SequenceId`, `ConversationSnapshot`, `project`, `Item`, `ContentPart`, `Model`, `ModelRequest`, `ModelEvent`, `ModelSettings`, `CancellationToken`, `TokenCounter`, `HeuristicTokenCounter` (all from core).
- Produces: `pub struct CompactingSession<S>`, `CompactingSession::builder(inner: S, model: Arc<dyn Model>) -> CompactingSessionBuilder<S>`; builder methods `.threshold(usize)`, `.token_counter(Arc<dyn TokenCounter>)`, `.prompt(impl Into<String>)`, `.model_settings(ModelSettings)`, `.build() -> Result<CompactingSession<S>, CompactingSessionError>`. (Re-exported from facade as `core::CompactingSession`.)

Read spec §4.2 for the full algorithm before implementing.

- [ ] **Step 1: Write the failing test file** (full behavior coverage)

Create `crates/paigasus-helikon-core/tests/compacting_session.rs`:

```rust
//! Behaviour tests for CompactingSession (spec §4.2, AC §11).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures_core::stream::BoxStream;
use paigasus_helikon_core::{
    CancellationToken, CompactingSession, ContentPart, FinishReason, HeuristicTokenCounter, Item,
    MemorySession, Model, ModelCapabilities, ModelError, ModelEvent, ModelRequest, Session,
    SessionEvent,
};

/// Fake model: returns a fixed summary, counting invocations.
#[derive(Clone)]
struct FakeModel {
    summary: String,
    calls: Arc<AtomicUsize>,
}
impl FakeModel {
    fn new(summary: &str) -> Self {
        Self { summary: summary.into(), calls: Arc::new(AtomicUsize::new(0)) }
    }
}
#[async_trait]
impl Model for FakeModel {
    async fn invoke(
        &self,
        _req: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let events = vec![
            Ok(ModelEvent::TokenDelta { text: self.summary.clone() }),
            Ok(ModelEvent::Finish { reason: FinishReason::Stop }),
        ];
        Ok(Box::pin(futures_util::stream::iter(events)))
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

/// Model that always errors on invoke.
struct ErrModel;
#[async_trait]
impl Model for ErrModel {
    async fn invoke(
        &self,
        _req: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<ModelEvent, ModelError>>, ModelError> {
        Err(ModelError::Unavailable)
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
}

fn user(text: &str) -> SessionEvent {
    SessionEvent::user_message(vec![ContentPart::Text { text: text.into() }])
}

// 1-token-per-char counter for exact threshold math in tests.
#[derive(Debug)]
struct CharCounter;
impl paigasus_helikon_core::TokenCounter for CharCounter {
    fn count(&self, items: &[Item]) -> usize {
        items
            .iter()
            .map(|i| match i {
                Item::UserMessage { content }
                | Item::System { content }
                | Item::AssistantMessage { content, .. } => content
                    .iter()
                    .map(|p| match p {
                        ContentPart::Text { text } => text.chars().count(),
                        _ => 0,
                    })
                    .sum(),
                _ => 0,
            })
            .sum()
    }
}

#[tokio::test]
async fn compacts_below_threshold_when_exceeded() {
    let model = FakeModel::new("S"); // 1-char summary
    let cs = CompactingSession::builder(MemorySession::new(), Arc::new(model.clone()))
        .token_counter(Arc::new(CharCounter))
        .threshold(10)
        .build()
        .unwrap();

    // Append > 10 chars of user text across two appends.
    cs.append(&[user("hello world")]).await.unwrap(); // 11 chars -> over threshold
    let snap = cs.snapshot().await.unwrap();
    assert_eq!(CharCounter.count(&snap.messages), 1, "snapshot reduced to the 1-char summary");
    assert!(matches!(snap.messages.as_slice(), [Item::System { .. }]));
    assert_eq!(model.calls.load(Ordering::SeqCst), 1, "summarized exactly once");
}

#[tokio::test]
async fn records_compacted_event_and_retains_raw_log() {
    let cs = CompactingSession::builder(MemorySession::new(), Arc::new(FakeModel::new("S")))
        .token_counter(Arc::new(CharCounter))
        .threshold(3)
        .build()
        .unwrap();
    cs.append(&[user("abcd")]).await.unwrap(); // 4 chars > 3
    let raw = cs.events(None).await.unwrap();
    // raw log: the user event + the appended Compacted marker
    assert_eq!(raw.len(), 2);
    assert!(matches!(raw[1], SessionEvent::Compacted { original_count: 1, .. }));
}

#[tokio::test]
async fn llm_error_is_swallowed_and_no_marker_appended() {
    let cs = CompactingSession::builder(MemorySession::new(), Arc::new(ErrModel))
        .token_counter(Arc::new(CharCounter))
        .threshold(3)
        .build()
        .unwrap();
    cs.append(&[user("abcd")]).await.unwrap(); // append still Ok
    let raw = cs.events(None).await.unwrap();
    assert_eq!(raw.len(), 1, "no Compacted marker appended on LLM failure");
}

#[tokio::test]
async fn empty_summary_appends_no_marker() {
    let cs = CompactingSession::builder(MemorySession::new(), Arc::new(FakeModel::new("   ")))
        .token_counter(Arc::new(CharCounter))
        .threshold(3)
        .build()
        .unwrap();
    cs.append(&[user("abcd")]).await.unwrap();
    let raw = cs.events(None).await.unwrap();
    assert_eq!(raw.len(), 1, "whitespace-only summary => no marker");
}

#[tokio::test]
async fn resume_over_threshold_compacts_on_first_append() {
    // Pre-populate an inner session ABOVE threshold, THEN wrap it.
    let inner = MemorySession::new();
    inner.append(&[user("0123456789")]).await.unwrap(); // 10 chars
    let cs = CompactingSession::builder(inner, Arc::new(FakeModel::new("S")))
        .token_counter(Arc::new(CharCounter))
        .threshold(5)
        .build()
        .unwrap();
    cs.append(&[user("x")]).await.unwrap(); // first append must seed + compact
    let snap = cs.snapshot().await.unwrap();
    assert_eq!(CharCounter.count(&snap.messages), 1, "resumed backlog compacted on first append");
}

#[tokio::test]
async fn threshold_zero_is_rejected() {
    let err = CompactingSession::builder(MemorySession::new(), Arc::new(FakeModel::new("S")))
        .threshold(0)
        .build();
    assert!(err.is_err());
}

#[tokio::test]
async fn lone_summary_over_threshold_is_not_recompacted() {
    // Inner already projects to a single, over-threshold System summary.
    let inner = MemorySession::new();
    inner
        .append(&[SessionEvent::compacted("LONG SUMMARY OVER THRESHOLD", 1)])
        .await
        .unwrap();
    let model = FakeModel::new("X");
    let cs = CompactingSession::builder(inner, Arc::new(model.clone()))
        .token_counter(Arc::new(CharCounter))
        .threshold(3) // summary (26 chars) is far above threshold
        .build()
        .unwrap();
    // A handoff contributes 0 projected messages, so the snapshot stays a lone
    // System summary (messages.len() == 1) -> the guard MUST skip compaction.
    cs.append(&[SessionEvent::handoff_occurred("a", "b")]).await.unwrap();
    assert_eq!(model.calls.load(Ordering::SeqCst), 0, "lone summary must not be re-compacted");
    let raw = cs.events(None).await.unwrap();
    assert_eq!(raw.len(), 2, "only the pre-seeded Compacted + the handoff; no new marker appended");
}
```

> Note for implementer: this test pins the `messages.len() <= 1` guard — a lone running summary that is itself over threshold must not trigger an endless re-summarization loop. The handoff (0 projected messages) is the vehicle for reaching `messages.len() == 1` on an append.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p paigasus-helikon-core --test compacting_session`
Expected: FAIL — `CompactingSession` not found.

- [ ] **Step 3: Implement `CompactingSession`** (spec §4.2)

Create `crates/paigasus-helikon-core/src/compacting_session.rs`:

```rust
//! [`CompactingSession`] — a [`Session`] wrapper that LLM-summarizes the log
//! once a token threshold is exceeded. See spec §4.2.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;

use crate::{
    project, CancellationToken, ContentPart, ConversationSnapshot, HeuristicTokenCounter, Item,
    Model, ModelEvent, ModelRequest, ModelSettings, SequenceId, Session, SessionError,
    SessionEvent, TokenCounter,
};

const DEFAULT_PROMPT: &str = "Summarize the conversation so far into a concise summary, \
preserving key facts, decisions, and open questions.";
/// Default threshold (tokens). Chosen with headroom under common context windows.
const DEFAULT_THRESHOLD: usize = 8_000;

/// A [`Session`] that wraps any inner session and triggers LLM-based
/// compaction once the projected token count exceeds `threshold`.
///
/// **Single logical writer per session** (spec §4.2): the inner backend stays
/// durable under concurrency, but the compaction bookkeeping assumes appends
/// through this wrapper are serialized. `threshold` must sit below the
/// summarization model's context window, and the model should produce
/// summaries shorter than `threshold`, for compaction to converge.
pub struct CompactingSession<S> {
    inner: S,
    model: Arc<dyn Model>,
    counter: Arc<dyn TokenCounter>,
    threshold: usize,
    settings: ModelSettings,
    prompt: String,
    cheap_estimate: AtomicUsize,
    compacting: AtomicBool,
}

// Manual Debug: `Arc<dyn Model>` is not Debug (Model has no Debug bound).
impl<S: std::fmt::Debug> std::fmt::Debug for CompactingSession<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompactingSession")
            .field("inner", &self.inner)
            .field("threshold", &self.threshold)
            .finish_non_exhaustive()
    }
}

/// Builder for [`CompactingSession`].
pub struct CompactingSessionBuilder<S> {
    inner: S,
    model: Arc<dyn Model>,
    counter: Option<Arc<dyn TokenCounter>>,
    threshold: usize,
    settings: ModelSettings,
    prompt: String,
}

impl<S: std::fmt::Debug> std::fmt::Debug for CompactingSessionBuilder<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompactingSessionBuilder")
            .field("inner", &self.inner)
            .field("threshold", &self.threshold)
            .finish_non_exhaustive()
    }
}

/// Error constructing a [`CompactingSession`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CompactingSessionError {
    /// `threshold` was 0 (would never compact).
    #[error("CompactingSession threshold must be greater than zero")]
    ZeroThreshold,
}

impl<S: Session> CompactingSession<S> {
    /// Start building a [`CompactingSession`] wrapping `inner`, summarizing via `model`.
    pub fn builder(inner: S, model: Arc<dyn Model>) -> CompactingSessionBuilder<S> {
        CompactingSessionBuilder {
            inner,
            model,
            counter: None,
            threshold: DEFAULT_THRESHOLD,
            settings: ModelSettings::default(),
            prompt: DEFAULT_PROMPT.to_owned(),
        }
    }
}

impl<S: Session> CompactingSessionBuilder<S> {
    /// Token threshold above which compaction fires. Must be > 0.
    pub fn threshold(mut self, t: usize) -> Self {
        self.threshold = t;
        self
    }
    /// Override the token counter (default [`HeuristicTokenCounter`]).
    pub fn token_counter(mut self, c: Arc<dyn TokenCounter>) -> Self {
        self.counter = Some(c);
        self
    }
    /// Override the summarization instruction prompt.
    pub fn prompt(mut self, p: impl Into<String>) -> Self {
        self.prompt = p.into();
        self
    }
    /// Override the model settings used for the summarization call.
    pub fn model_settings(mut self, s: ModelSettings) -> Self {
        self.settings = s;
        self
    }
    /// Build the [`CompactingSession`], or fail on an invalid configuration.
    pub fn build(self) -> Result<CompactingSession<S>, CompactingSessionError> {
        if self.threshold == 0 {
            return Err(CompactingSessionError::ZeroThreshold);
        }
        Ok(CompactingSession {
            inner: self.inner,
            model: self.model,
            counter: self.counter.unwrap_or_else(|| Arc::new(HeuristicTokenCounter)),
            threshold: self.threshold,
            settings: self.settings,
            prompt: self.prompt,
            // usize::MAX forces the first maybe_compact to take the authoritative
            // path and seed from the (possibly pre-populated) inner log.
            cheap_estimate: AtomicUsize::new(usize::MAX),
            compacting: AtomicBool::new(false),
        })
    }
}

/// RAII reset for the single-flight `compacting` flag — constructed only on the
/// swap-won path, so it never clears a flag it did not set.
struct CompactGuard<'a>(&'a AtomicBool);
impl Drop for CompactGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl<S: Session> CompactingSession<S> {
    fn add_estimate(&self, events: &[SessionEvent]) {
        // Cheap char estimate of new events (over-approx: counts handoffs as 0 text).
        let snap = project(events);
        let chars: usize = self.counter.count(&snap.messages) * 4; // counter returns ~chars/4
        let prev = self.cheap_estimate.load(Ordering::Relaxed);
        self.cheap_estimate
            .store(prev.saturating_add(chars), Ordering::Relaxed);
    }

    async fn maybe_compact(&self) -> Result<(), SessionError> {
        // 1. Cheap gate (usize::MAX on first call forces an authoritative read).
        if self.cheap_estimate.load(Ordering::Relaxed) <= self.threshold.saturating_mul(4) {
            return Ok(());
        }
        // 2. Single-flight.
        if self
            .compacting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        let _guard = CompactGuard(&self.compacting);

        // 3. Authoritative count.
        let evs = self.inner.events(None).await?;
        let snap = project(&evs);
        let tokens = self.counter.count(&snap.messages);
        if tokens <= self.threshold {
            self.cheap_estimate
                .store(tokens.saturating_mul(4), Ordering::Relaxed);
            return Ok(());
        }
        // 5. Nothing useful to collapse: empty, or a lone running System summary.
        //    (A single over-threshold non-summary message SHOULD still compact —
        //    `len <= 1` alone would wrongly skip it.)
        if snap.messages.is_empty()
            || (snap.messages.len() == 1 && matches!(snap.messages[0], Item::System { .. }))
        {
            return Ok(());
        }
        // 6. live = events since (and incl.) the last Compacted marker.
        let live = live_count(&evs);

        // 7. Summarize.
        let mut messages = snap.messages.clone();
        messages.push(Item::UserMessage {
            content: vec![ContentPart::Text { text: self.prompt.clone() }],
        });
        let req = ModelRequest {
            messages,
            tools: Vec::new(),
            model_settings: self.settings.clone(),
        };
        let summary = match self.collect_summary(req).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "CompactingSession: summarization failed; skipping compaction");
                return Ok(());
            }
        };
        // 8. Empty-summary guard.
        if summary.trim().is_empty() {
            tracing::warn!("CompactingSession: model returned empty summary; skipping compaction");
            return Ok(());
        }
        // 9. Append marker; resync cheap estimate to the summary size.
        self.inner
            .append(&[SessionEvent::compacted(summary.clone(), live as u64)])
            .await?;
        let summary_item = Item::System {
            content: vec![ContentPart::Text { text: summary }],
        };
        self.cheap_estimate
            .store(self.counter.count(&[summary_item]).saturating_mul(4), Ordering::Relaxed);
        Ok(())
    }

    async fn collect_summary(&self, req: ModelRequest) -> Result<String, SessionError> {
        let mut stream = self
            .model
            .invoke(req, CancellationToken::new())
            .await
            .map_err(|e| SessionError::Other(e.into()))?;
        let mut summary = String::new();
        while let Some(ev) = stream.next().await {
            match ev.map_err(|e| SessionError::Other(e.into()))? {
                ModelEvent::TokenDelta { text } => summary.push_str(&text),
                ModelEvent::Finish { .. } => break,
                _ => {}
            }
        }
        Ok(summary)
    }
}

/// Count of events since (and including) the last `Compacted`; full length if none.
fn live_count(evs: &[SessionEvent]) -> usize {
    let last_compacted = evs
        .iter()
        .rposition(|e| matches!(e, SessionEvent::Compacted { .. }));
    match last_compacted {
        Some(i) => evs.len() - i,
        None => evs.len(),
    }
}

#[async_trait]
impl<S: Session> Session for CompactingSession<S> {
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError> {
        self.inner.append(events).await?;
        self.add_estimate(events);
        self.maybe_compact().await?;
        Ok(())
    }
    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        self.inner.events(since).await
    }
    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        self.inner.snapshot().await
    }
}
```

> Implementer notes: `maybe_compact` returns `Ok(())` on summarization failure (best-effort), but a failure of the inner `events()`/`append()` (the durable layer) **does** propagate via `?`. Confirm `FinishReason` and `MemorySession` are re-exported from core root (they are). If `tracing::warn!(error = %e, …)` trips clippy over `SessionError: Display`, it is — `SessionError` derives `thiserror::Error`.

- [ ] **Step 4: Wire into `lib.rs`**

```rust
mod compacting_session;
pub use compacting_session::{CompactingSession, CompactingSessionBuilder, CompactingSessionError};
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p paigasus-helikon-core --test compacting_session`
Expected: PASS (all 7 tests).

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-core --all-targets -- -D warnings
git add crates/paigasus-helikon-core/src/compacting_session.rs crates/paigasus-helikon-core/src/lib.rs crates/paigasus-helikon-core/tests/compacting_session.rs
git commit -m "feat(core): SMA-330 add CompactingSession wrapper"
```

---

## Task 4: `paigasus-helikon-sessions-testkit` crate (conformance harness)

**Files:**
- Create: `crates/paigasus-helikon-sessions-testkit/Cargo.toml`
- Create: `crates/paigasus-helikon-sessions-testkit/src/lib.rs`
- Create: `crates/paigasus-helikon-sessions-testkit/tests/memory.rs`
- Create: `crates/paigasus-helikon-sessions-testkit/README.md`
- Modify: `release-plz.toml` (add `release = false` block)

**Interfaces:**
- Produces: `pub async fn run_append_read<F, Fut>(make: F)`, `run_watermark_exclusive`, `run_projection`, `run_concurrent_writers`, `run_all` — each `where F: Fn() -> Fut + Sync, Fut: Future<Output = Arc<dyn Session>> + Send`. (Consumed by Tasks 6, 9, 10.)

- [ ] **Step 1: Create the Cargo manifest**

`crates/paigasus-helikon-sessions-testkit/Cargo.toml`:

```toml
[package]
name        = "paigasus-helikon-sessions-testkit"
description = "Internal: shared Session conformance suite for Paigasus Helikon backends."
version     = "0.0.0"
publish     = false
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
futures-util          = { workspace = true }
tokio                 = { workspace = true, features = ["macros", "rt-multi-thread"] }
jiff                  = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 2: Write the harness** (document every `pub fn` — required by the `docs` gate)

`crates/paigasus-helikon-sessions-testkit/src/lib.rs`:

```rust
//! Shared `Session` conformance suite (spec §5). Each backend supplies a
//! factory that yields a fresh, empty session; these functions exercise the
//! append/read/projection/concurrency contract every backend must uphold.

use std::future::Future;
use std::sync::Arc;

use jiff::Timestamp;
use paigasus_helikon_core::{ContentPart, SequenceId, Session, SessionEvent};

fn user(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        content: vec![ContentPart::Text { text: text.into() }],
        ts: Timestamp::from_second(1_700_000_000).unwrap(),
    }
}

/// Append several events, read them back, assert order and count are preserved.
pub async fn run_append_read<F, Fut>(make: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn Session>>,
{
    let s = make().await;
    s.append(&[user("a"), user("b"), user("c")]).await.unwrap();
    let got = s.events(None).await.unwrap();
    assert_eq!(got.len(), 3, "all appended events read back");
    assert!(matches!(&got[0], SessionEvent::UserMessage { content, .. }
        if matches!(&content[0], ContentPart::Text { text } if text == "a")));
}

/// `events(Some(SequenceId(n)))` is an exclusive watermark: returns positions > n.
pub async fn run_watermark_exclusive<F, Fut>(make: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn Session>>,
{
    let s = make().await;
    for i in 0..5 {
        s.append(&[user(&format!("m{i}"))]).await.unwrap();
    }
    let after = s.events(Some(SequenceId(2))).await.unwrap();
    assert_eq!(after.len(), 2, "positions 3 and 4 only (exclusive of 2)");
}

/// `snapshot()` equals `project(events())`.
pub async fn run_projection<F, Fut>(make: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn Session>>,
{
    let s = make().await;
    s.append(&[user("hi")]).await.unwrap();
    let snap = s.snapshot().await.unwrap();
    let events = s.events(None).await.unwrap();
    let expected = paigasus_helikon_core::project(&events);
    assert_eq!(snap.messages.len(), expected.messages.len());
    assert_eq!(snap.messages.len(), 1);
}

/// N tasks append concurrently to the same session; every event survives once.
pub async fn run_concurrent_writers<F, Fut>(make: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn Session>>,
{
    const N_TASKS: usize = 16;
    const M_EVENTS: usize = 10;
    let session = make().await;
    let mut handles = Vec::new();
    for t in 0..N_TASKS {
        let s = session.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..M_EVENTS {
                s.append(&[user(&format!("t{t}-m{j}"))]).await.unwrap();
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    let all = session.events(None).await.unwrap();
    assert_eq!(all.len(), N_TASKS * M_EVENTS, "no lost or duplicated events");
    let mut texts: Vec<String> = all
        .into_iter()
        .filter_map(|e| match e {
            SessionEvent::UserMessage { content, .. } => match content.into_iter().next() {
                Some(ContentPart::Text { text }) => Some(text),
                _ => None,
            },
            _ => None,
        })
        .collect();
    texts.sort();
    let mut expected: Vec<String> = (0..N_TASKS)
        .flat_map(|t| (0..M_EVENTS).map(move |j| format!("t{t}-m{j}")))
        .collect();
    expected.sort();
    assert_eq!(texts, expected, "every sent event present exactly once");
}

/// Run the full conformance suite against `make`. `make` is invoked once per
/// sub-test and MUST return a fresh, empty session each time.
pub async fn run_all<F, Fut>(make: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Arc<dyn Session>>,
{
    run_append_read(&make).await;
    run_watermark_exclusive(&make).await;
    run_projection(&make).await;
    run_concurrent_writers(&make).await;
}
```

> Note: to allow `run_all(&make)` to forward `&make` to each sub-fn, the sub-fns take `F: Fn`; `&F` is also `Fn`, so `run_append_read(&make)` type-checks. Verify; if the borrow forwarding fights inference, change `run_all` to call each sub-fn with a closure `|| make()`.

- [ ] **Step 3: Anchor against `MemorySession`**

`crates/paigasus-helikon-sessions-testkit/tests/memory.rs`:

```rust
use std::sync::Arc;
use paigasus_helikon_core::{MemorySession, Session};
use paigasus_helikon_sessions_testkit::run_all;

#[tokio::test]
async fn memory_session_passes_conformance() {
    run_all(|| async { Arc::new(MemorySession::new()) as Arc<dyn Session> }).await;
}
```

- [ ] **Step 4: README + release-plz block**

`crates/paigasus-helikon-sessions-testkit/README.md`:

```markdown
# paigasus-helikon-sessions-testkit

Internal, unpublished (`publish = false`) crate housing the shared `Session`
conformance suite (append / read / watermark / projection / concurrent
writers). Consumed as a path-only dev-dependency by the session backend crates.
Not part of the public API.
```

In `release-plz.toml`, add alongside the other stub blocks:

```toml
[[package]]
name = "paigasus-helikon-sessions-testkit"
publish = false
release = false
```

- [ ] **Step 5: Run + gates + commit**

```bash
cargo test -p paigasus-helikon-sessions-testkit
cargo fmt --all
cargo clippy -p paigasus-helikon-sessions-testkit --all-targets -- -D warnings
git add crates/paigasus-helikon-sessions-testkit release-plz.toml
git commit -m "feat(sessions-testkit): SMA-330 add shared Session conformance suite"
```

---

## Task 5: SQLite retrofit — accessors + conformance

**Files:**
- Modify: `crates/paigasus-helikon-sessions-sqlite/src/lib.rs:200-221` (replace `event_metadata`)
- Modify: `crates/paigasus-helikon-sessions-sqlite/Cargo.toml` (`[dev-dependencies]`)
- Create: `crates/paigasus-helikon-sessions-sqlite/tests/conformance.rs`

**Interfaces:**
- Consumes: `SessionEvent::kind`, `SessionEvent::ts_nanos_saturating` (Task 1); `run_all` (Task 4).

- [ ] **Step 1: Add the testkit dev-dependency**

In `crates/paigasus-helikon-sessions-sqlite/Cargo.toml` `[dev-dependencies]` add (path-only, no version):

```toml
paigasus-helikon-sessions-testkit = { path = "../paigasus-helikon-sessions-testkit" }
```

- [ ] **Step 2: Write the failing conformance test**

`crates/paigasus-helikon-sessions-sqlite/tests/conformance.rs`:

```rust
//! SQLite runs the shared conformance suite (spec §5).

use std::sync::Arc;
use std::time::Duration;

use paigasus_helikon_core::Session;
use paigasus_helikon_sessions_sqlite::SqliteSession;
use paigasus_helikon_sessions_testkit::run_all;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_passes_conformance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conf.db");
    let opts = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(30));
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await
        .unwrap();
    SqliteSession::migrate(&pool).await.unwrap();

    // Unique session id per make() call -> fresh empty session each time.
    let counter = std::sync::atomic::AtomicU64::new(0);
    run_all(|| {
        let pool = pool.clone();
        let id = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        async move {
            Arc::new(SqliteSession::open_without_migrate(pool, format!("conf-{id}")))
                as Arc<dyn Session>
        }
    })
    .await;
    // keep `dir` alive until here
    drop(dir);
}
```

- [ ] **Step 3: Run to verify it passes** (the suite should already pass against the unchanged backend)

Run: `cargo test -p paigasus-helikon-sessions-sqlite --test conformance`
Expected: PASS. (If the closure-borrow issue from Task 4 surfaces, adjust per that note.)

- [ ] **Step 4: Refactor `event_metadata` onto the accessors**

Replace the body of `event_metadata` in `crates/paigasus-helikon-sessions-sqlite/src/lib.rs` (the `match` with the `_ => panic!`) with:

```rust
fn event_metadata(ev: &SessionEvent) -> (&'static str, i64) {
    (ev.kind(), ev.ts_nanos_saturating())
}
```

- [ ] **Step 5: Run the full sqlite suite to verify the refactor**

Run: `cargo test -p paigasus-helikon-sessions-sqlite`
Expected: PASS (roundtrip, persistence, multi_session, concurrent_writers, conformance).

- [ ] **Step 6: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-sessions-sqlite --all-targets -- -D warnings
git add crates/paigasus-helikon-sessions-sqlite
git commit -m "refactor(sessions-sqlite): SMA-330 use SessionEvent accessors and shared conformance suite"
```

---

## Task 6: PR-1 docs (mdBook Sessions page)

**Files:**
- Modify: `docs/book/src/` Sessions concept page (find it: `ls docs/book/src` / grep "Sessions")
- Modify: `crates/paigasus-helikon-sessions-sqlite/README.md` only if its public surface changed (it did not — likely no edit)

- [ ] **Step 1: Locate the Sessions page**

Run: `grep -ril "session" docs/book/src`

- [ ] **Step 2: Document `CompactingSession` + the token-threshold model**

Add a "Compaction" subsection to the Sessions page describing: `CompactingSession<S>` wraps any `Session`; fires LLM summarization once a `TokenCounter` estimate exceeds the threshold; records `SessionEvent::Compacted`; the running-summary model (full-history collapse; provider-translator caveat — summaries render as `Item::System`). Keep prose consistent with spec §4.2. Mention the shared conformance suite as the backend contract.

- [ ] **Step 3: Verify the book builds clean**

Run: `mdbook build docs/book`
Expected: success, no linkcheck errors. (If `mdbook` is absent: `cargo install mdbook mdbook-linkcheck`.)

- [ ] **Step 4: Commit**

```bash
git add docs/book
git commit -m "docs(book): SMA-330 document CompactingSession and session conformance"
```

---

## Task 7: PR-1 full-gate verification + open PR

- [ ] **Step 1: Run every CI gate locally**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```
Expected: all clean. (doc-coverage runs on nightly in CI; the `docs` job above catches missing `///`.)

- [ ] **Step 2: Push and open PR-1**

```bash
git push -u origin feature/sma-330-sessions-core-compaction-testkit
gh pr create --title "feat(core): SMA-330 add CompactingSession, token counting, and shared session conformance suite" \
  --body "<see body template below>"
```

PR-1 body must: reference SMA-330 **without** a "Closes" keyword (PR-2 closes it — spec §10); summarize the change (core: `CompactingSession`, `TokenCounter`, `SessionEvent` accessors; new unpublished testkit; sqlite retrofit); note this is **PR 1 of 2** and that PR-2 (postgres/redis) follows after PR-1's core publishes; describe how it was tested (the four-gate run above). Verify the title satisfies pr-title.yml: `type(scope):` prefix + lowercase subject after `SMA-330`.

---

# PR-2 — postgres + redis backends

**Precondition: PR-1 is merged AND its release-plz `chore: release` PR has merged so `paigasus-helikon-core` (with the new accessors) is published to crates.io.** Branch `feature/sma-330-sessions-postgres-redis` off the updated `main`. Also: after PR-1 merged, move SMA-330 back to **In Progress** (it auto-closed) — spec §10.

## Task 8: PostgresSession crate

**Files:**
- Create: `crates/paigasus-helikon-sessions-postgres/Cargo.toml`
- Create: `crates/paigasus-helikon-sessions-postgres/src/lib.rs`
- Create: `crates/paigasus-helikon-sessions-postgres/migrations/0001_session_events.sql`
- Create: `crates/paigasus-helikon-sessions-postgres/README.md`
- Create: `crates/paigasus-helikon-sessions-postgres/tests/conformance.rs`
- Modify: root `Cargo.toml` (`[workspace.dependencies]`)

**Interfaces:**
- Produces: `PostgresSession` with `migrate(&PgPool)`, `open(PgPool, impl Into<String>)`, `open_without_migrate(PgPool, impl Into<String>)`, `session_id(&self) -> &str`, and `impl Session`.

Mirror `crates/paigasus-helikon-sessions-sqlite/src/lib.rs`. Read spec §6 (single transaction, advisory lock, runtime `query()` only, JSONB, aws-lc-rs TLS).

- [ ] **Step 1: Cargo manifest**

```toml
[package]
name        = "paigasus-helikon-sessions-postgres"
description = "PostgreSQL-backed Session backend for the Paigasus Helikon AI SDK."
version     = "0.1.0"
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
serde_json            = { workspace = true }
# adds the `postgres` driver + aws-lc-rs rustls TLS to the workspace sqlx base.
# VERIFY the exact 0.9 TLS feature name (likely `tls-rustls-aws-lc-rs`); must be
# aws-lc-rs (NOT ring) to avoid the dual-CryptoProvider panic. Spec §6.
sqlx                  = { workspace = true, features = ["postgres", "tls-rustls-aws-lc-rs"] }

[dev-dependencies]
paigasus-helikon-sessions-testkit = { path = "../paigasus-helikon-sessions-testkit" }
tokio                             = { workspace = true, features = ["macros", "rt-multi-thread"] }

[lints]
workspace = true
```

- [ ] **Step 2: Migration**

`migrations/0001_session_events.sql`:

```sql
CREATE TABLE IF NOT EXISTS session_events (
    session_id TEXT   NOT NULL,
    sequence   BIGINT NOT NULL,
    ts_nanos   BIGINT NOT NULL,
    kind       TEXT   NOT NULL,
    payload    JSONB  NOT NULL,
    PRIMARY KEY (session_id, sequence)
);
CREATE INDEX IF NOT EXISTS idx_session_events_session_ts
    ON session_events (session_id, ts_nanos);
```

- [ ] **Step 3: Implementation**

`src/lib.rs` (crate docs + the type; document every `pub` item):

```rust
//! PostgreSQL-backed [`Session`] implementation. Mirrors the sqlite backend's
//! event-log shape; safe for concurrent writers via a per-session advisory lock.
//!
//! [`Session`]: paigasus_helikon_core::Session

use async_trait::async_trait;
use paigasus_helikon_core::{
    project, ConversationSnapshot, SequenceId, Session, SessionError, SessionEvent,
};
use sqlx::PgPool;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// PostgreSQL-backed [`Session`]. One instance is one session (`session_id`);
/// pools are shared across instances.
#[derive(Debug, Clone)]
pub struct PostgresSession {
    pool: PgPool,
    session_id: String,
}

impl PostgresSession {
    /// Run embedded migrations on `pool`. Idempotent.
    pub async fn migrate(pool: &PgPool) -> Result<(), SessionError> {
        MIGRATOR.run(pool).await.map_err(SessionError::backend)?;
        Ok(())
    }

    /// Open (and migrate) a session within `pool`.
    pub async fn open(pool: PgPool, session_id: impl Into<String>) -> Result<Self, SessionError> {
        Self::migrate(&pool).await?;
        Ok(Self::open_without_migrate(pool, session_id))
    }

    /// Open a session without running migrations (caller migrated already).
    pub fn open_without_migrate(pool: PgPool, session_id: impl Into<String>) -> Self {
        Self { pool, session_id: session_id.into() }
    }

    /// The `session_id` this instance reads and writes.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

#[async_trait]
impl Session for PostgresSession {
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError> {
        if events.is_empty() {
            return Ok(());
        }
        // Single transaction on ONE pooled connection: the advisory lock must
        // cover the INSERTs. Per-session lock auto-releases at COMMIT.
        let mut tx = self.pool.begin().await.map_err(SessionError::backend)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(&self.session_id)
            .execute(&mut *tx)
            .await
            .map_err(SessionError::backend)?;
        let next: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sequence), -1) + 1 FROM session_events WHERE session_id = $1",
        )
        .bind(&self.session_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(SessionError::backend)?;

        for (offset, ev) in events.iter().enumerate() {
            let seq = next + offset as i64;
            let payload = serde_json::to_value(ev).map_err(SessionError::backend)?;
            sqlx::query(
                "INSERT INTO session_events (session_id, sequence, ts_nanos, kind, payload) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(&self.session_id)
            .bind(seq)
            .bind(ev.ts_nanos_saturating())
            .bind(ev.kind())
            .bind(payload)
            .execute(&mut *tx)
            .await
            .map_err(SessionError::backend)?;
        }
        tx.commit().await.map_err(SessionError::backend)?;
        Ok(())
    }

    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        let watermark: i64 = match since {
            Some(s) => i64::try_from(s.0).unwrap_or(i64::MAX),
            None => -1,
        };
        let rows: Vec<(sqlx::types::Json<SessionEvent>,)> = sqlx::query_as(
            "SELECT payload FROM session_events \
             WHERE session_id = $1 AND sequence > $2 ORDER BY sequence",
        )
        .bind(&self.session_id)
        .bind(watermark)
        .fetch_all(&self.pool)
        .await
        .map_err(SessionError::backend)?;
        Ok(rows.into_iter().map(|(j,)| j.0).collect())
    }

    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        Ok(project(&self.events(None).await?))
    }
}
```

> Verify: `sqlx::query_scalar`/`query_as` with `Json<SessionEvent>` requires `SessionEvent: Deserialize` (it is). Runtime `query()` only — no `sqlx::query!`.

- [ ] **Step 4: Conformance test (env-gated loud-skip)**

`tests/conformance.rs`:

```rust
//! Postgres conformance — runs only when HELIKON_TEST_POSTGRES_URL is set
//! (loud-skips otherwise, like forkd_live). spec §9.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use paigasus_helikon_core::Session;
use paigasus_helikon_sessions_postgres::PostgresSession;
use paigasus_helikon_sessions_testkit::run_all;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn postgres_passes_conformance() {
    let Ok(url) = std::env::var("HELIKON_TEST_POSTGRES_URL") else {
        eprintln!("SKIP postgres_passes_conformance: HELIKON_TEST_POSTGRES_URL unset");
        return;
    };
    let pool = sqlx::PgPool::connect(&url).await.expect("connect");
    PostgresSession::migrate(&pool).await.expect("migrate"); // migrate ONCE up front
    let counter = AtomicU64::new(0);
    run_all(|| {
        let pool = pool.clone();
        let id = counter.fetch_add(1, Ordering::SeqCst);
        async move {
            Arc::new(PostgresSession::open_without_migrate(pool, format!("conf-{id}")))
                as Arc<dyn Session>
        }
    })
    .await;
}
```

- [ ] **Step 5: README + workspace dep**

`README.md`: install via `cargo add paigasus-helikon-sessions-postgres`; quickstart (build a `PgPool`, `PostgresSession::open(pool, id)`); note JSONB event-log shape, per-session advisory-lock concurrency, and aws-lc-rs TLS. In root `Cargo.toml` `[workspace.dependencies]` add:

```toml
paigasus-helikon-sessions-postgres = { path = "crates/paigasus-helikon-sessions-postgres", version = "0.1.0" }
```

- [ ] **Step 6: Verify (with and without a server) + commit**

```bash
cargo build -p paigasus-helikon-sessions-postgres            # lib-only, no server
cargo test -p paigasus-helikon-sessions-postgres             # loud-skips without env
# Optional local server run (if Docker available):
#   docker run --rm -d -e POSTGRES_PASSWORD=postgres -p 5432:5432 postgres:17
#   HELIKON_TEST_POSTGRES_URL=postgres://postgres:postgres@localhost:5432/postgres cargo test -p paigasus-helikon-sessions-postgres
cargo fmt --all
cargo clippy -p paigasus-helikon-sessions-postgres --all-targets -- -D warnings
git add crates/paigasus-helikon-sessions-postgres Cargo.toml
git commit -m "feat(sessions-postgres): SMA-330 add PostgreSQL Session backend"
```

---

## Task 9: RedisSession crate

**Files:**
- Create: `crates/paigasus-helikon-sessions-redis/Cargo.toml`
- Create: `crates/paigasus-helikon-sessions-redis/src/lib.rs`
- Create: `crates/paigasus-helikon-sessions-redis/README.md`
- Create: `crates/paigasus-helikon-sessions-redis/tests/conformance.rs`
- Modify: root `Cargo.toml` (`[workspace.dependencies]` — add `redis` + the crate)

**Interfaces:**
- Produces: `RedisSession` with `new(redis::aio::ConnectionManager, impl Into<String>)`, `connect(url, id)` (plaintext convenience), `session_id()`, and `impl Session`.

Read spec §7 (one Stream per session, Lua `XLEN→XADD` for contiguous seq, no TLS feature, no trim).

- [ ] **Step 1: Cargo manifest** (confirm `redis` latest version + feature names)

```toml
[package]
name        = "paigasus-helikon-sessions-redis"
description = "Redis-backed Session backend (Redis Streams) for the Paigasus Helikon AI SDK."
version     = "0.1.0"
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
serde_json            = { workspace = true }
redis                 = { workspace = true }

[dev-dependencies]
paigasus-helikon-sessions-testkit = { path = "../paigasus-helikon-sessions-testkit" }
tokio                             = { workspace = true, features = ["macros", "rt-multi-thread"] }

[lints]
workspace = true
```

Add to root `Cargo.toml` `[workspace.dependencies]` (NO rustls/TLS feature — spec §7):

```toml
redis = { version = "<latest>", default-features = false, features = ["tokio-comp", "connection-manager", "streams", "script"] }
paigasus-helikon-sessions-redis = { path = "crates/paigasus-helikon-sessions-redis", version = "0.1.0" }
```

- [ ] **Step 2: Implementation**

`src/lib.rs` (document every `pub` item; read spec §7):

```rust
//! Redis-backed [`Session`] — one Redis Stream per session. Contiguous
//! sequence numbers are allocated atomically via a Lua `XLEN`→`XADD` script.
//!
//! TLS: this crate enables no rustls feature; for managed Redis, build a
//! TLS-configured [`redis::aio::ConnectionManager`] yourself and pass it to
//! [`RedisSession::new`]. The stream is never trimmed (the contiguous-seq
//! invariant requires it) — run the keyspace with `noeviction`. spec §7.
//!
//! [`Session`]: paigasus_helikon_core::Session

use async_trait::async_trait;
use paigasus_helikon_core::{
    project, ConversationSnapshot, SequenceId, Session, SessionError, SessionEvent,
};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;

/// Redis-backed [`Session`]. One instance is one session (`session_id`).
#[derive(Clone)]
pub struct RedisSession {
    conn: ConnectionManager,
    session_id: String,
    key: String,
}

impl std::fmt::Debug for RedisSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisSession").field("session_id", &self.session_id).finish_non_exhaustive()
    }
}

impl RedisSession {
    /// Wrap an existing connection. Use this with a TLS-configured manager for
    /// managed Redis.
    pub fn new(conn: ConnectionManager, session_id: impl Into<String>) -> Self {
        let session_id = session_id.into();
        let key = format!("helikon:session:{session_id}:events");
        Self { conn, session_id, key }
    }

    /// Connect to `url` (plaintext) and open a session. For TLS, build your own
    /// [`ConnectionManager`] and use [`RedisSession::new`].
    pub async fn connect(url: &str, session_id: impl Into<String>) -> Result<Self, SessionError> {
        let client = redis::Client::open(url).map_err(SessionError::backend)?;
        let conn = ConnectionManager::new(client).await.map_err(SessionError::backend)?;
        Ok(Self::new(conn, session_id))
    }

    /// The `session_id` this instance reads and writes.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

// Atomic contiguous-seq append: XLEN gives the next seq; the whole script runs
// atomically (Redis single-threaded), so concurrent appends can't collide.
const APPEND_SCRIPT: &str = r#"
local n = redis.call('XLEN', KEYS[1])
for i = 0, (#ARGV / 3) - 1 do
  redis.call('XADD', KEYS[1], '*',
    'seq', n + i, 'kind', ARGV[i*3 + 1], 'payload', ARGV[i*3 + 2], 'ts', ARGV[i*3 + 3])
end
return n
"#;

#[async_trait]
impl Session for RedisSession {
    async fn append(&self, events: &[SessionEvent]) -> Result<(), SessionError> {
        if events.is_empty() {
            return Ok(());
        }
        let script = redis::Script::new(APPEND_SCRIPT);
        let mut invocation = script.key(&self.key);
        // bind args in (kind, payload, ts) triples
        let mut owned: Vec<String> = Vec::with_capacity(events.len() * 3);
        for ev in events {
            let payload = serde_json::to_string(ev).map_err(SessionError::backend)?;
            owned.push(ev.kind().to_owned());
            owned.push(payload);
            owned.push(ev.ts_nanos_saturating().to_string());
        }
        for a in &owned {
            invocation = invocation.arg(a);
        }
        let mut conn = self.conn.clone();
        let _: i64 = invocation.invoke_async(&mut conn).await.map_err(SessionError::backend)?;
        Ok(())
    }

    async fn events(&self, since: Option<SequenceId>) -> Result<Vec<SessionEvent>, SessionError> {
        let watermark: i64 = match since {
            Some(s) => i64::try_from(s.0).unwrap_or(i64::MAX),
            None => -1,
        };
        let mut conn = self.conn.clone();
        // XRANGE returns entries in insertion order; parse seq + payload fields.
        let entries: redis::streams::StreamRangeReply =
            conn.xrange(&self.key, "-", "+").await.map_err(SessionError::backend)?;
        let mut out = Vec::new();
        for entry in entries.ids {
            let seq: i64 = entry
                .get("seq")
                .ok_or_else(|| SessionError::backend(MissingField("seq")))?;
            if seq <= watermark {
                continue;
            }
            let payload: String = entry
                .get("payload")
                .ok_or_else(|| SessionError::backend(MissingField("payload")))?;
            out.push(serde_json::from_str::<SessionEvent>(&payload).map_err(SessionError::backend)?);
        }
        Ok(out)
    }

    async fn snapshot(&self) -> Result<ConversationSnapshot, SessionError> {
        Ok(project(&self.events(None).await?))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("redis stream entry missing field: {0}")]
struct MissingField(&'static str);
```

> Verify at impl time: `redis::streams::StreamRangeReply` / `StreamId::get` typed accessors (`get::<i64>`/`get::<String>`) — exact API of the pinned `redis` version; the `thiserror` dep must be added to `[dependencies]` if `MissingField` uses it (add `thiserror = { workspace = true }`). Adjust the error wrapping if `redis`'s reply types differ.

- [ ] **Step 3: Conformance test (env-gated)**

`tests/conformance.rs` — mirror Task 8 Step 4 with `HELIKON_TEST_REDIS_URL` and `RedisSession::connect(&url, format!("conf-{id}-{nonce}"))`. Use a **process-unique key prefix** (e.g. include a random-ish nonce from the env or a static counter + timestamp passed in) so repeated CI runs against a persistent Redis don't collide. Since `Math::random`-style nondeterminism isn't needed, a per-run unique id can come from `std::process::id()` + the counter:

```rust
let nonce = std::process::id();
// ... format!("conf-{nonce}-{id}")
```

- [ ] **Step 4: README + commit**

`README.md`: `cargo add paigasus-helikon-sessions-redis`; quickstart (`RedisSession::connect("redis://…", id)`); note Redis-Streams storage, atomic contiguous seq, BYO-`ConnectionManager` for TLS, and the no-trim/`noeviction` operational caveat.

```bash
cargo build -p paigasus-helikon-sessions-redis
cargo test -p paigasus-helikon-sessions-redis    # loud-skips without env
cargo fmt --all
cargo clippy -p paigasus-helikon-sessions-redis --all-targets -- -D warnings
git add crates/paigasus-helikon-sessions-redis Cargo.toml
git commit -m "feat(sessions-redis): SMA-330 add Redis Streams Session backend"
```

---

## Task 10: Facade wiring

**Files:**
- Modify: `crates/paigasus-helikon/Cargo.toml` (optional deps + features)
- Modify: `crates/paigasus-helikon/src/lib.rs` (re-exports)

- [ ] **Step 1: Add optional deps + features**

In `crates/paigasus-helikon/Cargo.toml` `[dependencies]`:

```toml
paigasus-helikon-sessions-postgres = { workspace = true, optional = true }
paigasus-helikon-sessions-redis    = { workspace = true, optional = true }
```

In `[features]`:

```toml
sessions-postgres = ["dep:paigasus-helikon-sessions-postgres"]
sessions-redis    = ["dep:paigasus-helikon-sessions-redis"]
```

- [ ] **Step 2: Add re-exports** (each needs `///` — missing_docs)

In `crates/paigasus-helikon/src/lib.rs`, next to the sqlite re-export:

```rust
/// PostgreSQL-backed `Session` backend. Enabled via the `sessions-postgres` feature.
#[cfg(feature = "sessions-postgres")]
pub use paigasus_helikon_sessions_postgres as sessions_postgres;

/// Redis-backed `Session` backend. Enabled via the `sessions-redis` feature.
#[cfg(feature = "sessions-redis")]
pub use paigasus_helikon_sessions_redis as sessions_redis;
```

- [ ] **Step 3: Verify feature builds + commit**

```bash
cargo build -p paigasus-helikon --features sessions-postgres
cargo build -p paigasus-helikon --features sessions-redis
cargo fmt --all
cargo clippy -p paigasus-helikon --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon/Cargo.toml crates/paigasus-helikon/src/lib.rs
git commit -m "feat(facade): SMA-330 expose sessions-postgres and sessions-redis features"
```

---

## Task 11: CI `sessions-it` job (required, path-filtered)

**Files:**
- Modify: `.github/workflows/ci.yml` (add the `sessions-it` job)
- Modify: `.github/rulesets/main-protection-checks.json` (add `sessions-it` context)

Read spec §9. The job ALWAYS runs (so the required context always reports) but starts containers only when session paths changed.

- [ ] **Step 1: Resolve latest action SHAs**

For `dorny/paths-filter` and any retry action, resolve the latest release tag → commit SHA (CLAUDE.md action-pinning recipe):

```bash
gh api repos/dorny/paths-filter/releases/latest | jq -r '.tag_name'
gh api repos/dorny/paths-filter/git/ref/tags/<tag> | jq -r '.object.sha'
```

Reuse the existing pinned SHAs for `actions/checkout`, `dtolnay/rust-toolchain`, `Swatinem/rust-cache` from `ci.yml`.

- [ ] **Step 2: Add the job** (Docker only when relevant)

Append to `.github/workflows/ci.yml` `jobs:` — an always-running job with an in-job path filter; on session changes it starts digest-pinned Postgres/Redis and runs the suite (retry-wrapped). Pattern (fill in resolved SHAs + a current image digest):

```yaml
  sessions-it:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@<pinned-sha>
        with: { persist-credentials: false }
      - uses: dorny/paths-filter@<pinned-sha>
        id: filter
        with:
          filters: |
            sessions:
              - 'crates/paigasus-helikon-sessions-**'
              - 'crates/paigasus-helikon-core/src/session.rs'
              - '.github/workflows/ci.yml'
              - 'Cargo.lock'
      - if: steps.filter.outputs.sessions == 'false'
        run: echo "no session-related changes; nothing to run"
      - if: steps.filter.outputs.sessions == 'true'
        uses: dtolnay/rust-toolchain@<pinned-sha>
        with: { toolchain: stable }
      - if: steps.filter.outputs.sessions == 'true'
        uses: Swatinem/rust-cache@<pinned-sha>
      - if: steps.filter.outputs.sessions == 'true'
        name: start services
        run: |
          docker run -d --name pg  -e POSTGRES_PASSWORD=postgres -p 5432:5432 postgres@sha256:<digest>
          docker run -d --name rds -p 6379:6379 redis@sha256:<digest>
          # wait for health
          for i in $(seq 1 30); do pg_isready -h localhost -p 5432 && break; sleep 2; done
          for i in $(seq 1 30); do redis-cli -h localhost ping && break; sleep 2; done
      - if: steps.filter.outputs.sessions == 'true'
        name: run integration suite (retry)
        env:
          HELIKON_TEST_POSTGRES_URL: postgres://postgres:postgres@localhost:5432/postgres
          HELIKON_TEST_REDIS_URL: redis://localhost:6379
        run: |
          for attempt in 1 2 3; do
            cargo test -p paigasus-helikon-sessions-postgres -p paigasus-helikon-sessions-redis && exit 0
            echo "attempt $attempt failed; retrying"; sleep 5
          done
          exit 1
```

> Prefer GHCR-mirrored images / digest pins to dodge Docker Hub anon rate limits (spec §9). `postgres:17`/`redis:7` are the tag baseline; resolve current digests at impl time.

- [ ] **Step 3: Add the required-status context**

Add `"sessions-it"` to the required-checks list in `.github/rulesets/main-protection-checks.json` (match the existing array's shape). Note in the PR body that a maintainer must **apply** the ruleset after merge (it is a mirror, not auto-applied) and rebase any open PRs through the transition (spec §9).

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml .github/rulesets/main-protection-checks.json
git commit -m "chore(workflows): SMA-330 add required sessions-it integration job"
```

---

## Task 12: Supply-chain + MSRV verification

**Files:** possibly `deny.toml` (license allowlist), possibly root `Cargo.toml` (`rust-version`).

- [ ] **Step 1: Vet the new dependency graph**

```bash
cargo update           # refresh lockfile for the new crates
cargo deny check       # licenses + advisories for redis + sqlx-postgres transitives
cargo audit
```
If a new license appears (e.g. an unlisted MIT/BSD transitive), add it to `deny.toml` `[licenses].allow`. If an advisory appears, pin/patch via `[workspace.dependencies]`.

- [ ] **Step 2: MSRV check**

```bash
cargo msrv --path crates/paigasus-helikon-sessions-postgres verify || true
cargo +1.94 build -p paigasus-helikon-sessions-postgres -p paigasus-helikon-sessions-redis
```
If 1.94 fails to build, raise `[workspace.package].rust-version` to what cargo demands (and the CI `1.94` matrix label) — never downgrade the dep (spec §11, CLAUDE.md).

- [ ] **Step 3: Commit any changes**

```bash
git add deny.toml Cargo.toml Cargo.lock
git commit -m "chore(deps): SMA-330 vet redis and sqlx-postgres supply chain"
```

---

## Task 13: PR-2 docs (READMEs + mdBook roster)

**Files:**
- Modify: `crates/paigasus-helikon/README.md`, root `README.md` (crate roster + feature→module map)
- Modify: `docs/book/src/` backends/roster page(s)

- [ ] **Step 1: Update facade + root README roster**

Add `sessions-postgres` and `sessions-redis` to the feature→module map and crate roster tables; keep `cargo add` install snippets (no hardcoded versions).

- [ ] **Step 2: Update the mdBook**

Document the Postgres and Redis backends on the Sessions/backends page (storage shape, concurrency model, TLS story, operational caveats). Ensure `mdbook build docs/book` is clean.

- [ ] **Step 3: Commit**

```bash
git add crates/paigasus-helikon/README.md README.md docs/book
git commit -m "docs: SMA-330 document Postgres and Redis session backends"
```

---

## Task 14: PR-2 full-gate verification + open PR

- [ ] **Step 1: Run every gate locally**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features          # postgres/redis loud-skip without env
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
mdbook build docs/book
```

If Docker is available locally, also run the env-set integration tests once (Task 8/9 optional commands).

- [ ] **Step 2: Push + open PR-2**

```bash
git push -u origin feature/sma-330-sessions-postgres-redis
gh pr create --title "feat(sessions): SMA-330 add Postgres and Redis production session backends" --body "<body>"
```

PR-2 body: **Closes SMA-330**; summarize the two backends + facade features + required `sessions-it` job; note the post-merge maintainer steps (apply ruleset, rebase open PRs); describe testing (local gates + the `sessions-it` run on the PR). Confirm the title passes pr-title.yml.

---

## Self-Review checklist (run before handing off)

- [ ] **Spec coverage:** §4 (Tasks 1-3) · §5 testkit (Task 4) · §5 sqlite retrofit (Task 5) · §6 Postgres (Task 8) · §7 Redis (Task 9) · §8 facade/deps (Tasks 8,9,10,12) · §9 CI required (Task 11) · §10 two-PR/release (Task 0, headers, Tasks 7/14 PR bodies) · §11 testing/MSRV (per-task tests + Task 12) · §12 docs (Tasks 6,13). All mapped.
- [ ] **Placeholders:** the only intentional `<…>` are external-lookup values (sqlx TLS feature name, `redis` version, action SHAs, image digests, PR body) — each flagged "verify/resolve at impl time," not hand-wavy logic.
- [ ] **Type consistency:** `make: Fn() -> Fut<Output = Arc<dyn Session>>` is identical across testkit + all consumers; `event_metadata` → `(ev.kind(), ev.ts_nanos_saturating())` matches Task 1's signatures; `CompactingSession::builder(...).build() -> Result<_, CompactingSessionError>` consistent across Task 3 tests + impl.
