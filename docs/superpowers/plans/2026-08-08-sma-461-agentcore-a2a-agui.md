# SMA-461 — AgentCore A2A and AG-UI protocol shims Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add AWS Bedrock AgentCore's A2A and AG-UI protocol modes to `paigasus-helikon-runtime-agentcore`, plus the optional WebSocket `/ws` endpoint on its existing HTTP mode.

**Architecture:** Three new feature-gated protocol surfaces hang off the existing `AgentCoreServer`, each with a pure `*_router()` (testable via `tower::ServiceExt::oneshot`) and a `serve_*()` that binds the protocol's fixed port. Every mode delegates execution to the configured `Runner` and reuses the crate's existing `PingState` and session-id extraction. A2A adds a public `TaskStore` trait with a bounded in-memory default; AG-UI adds a stateful `AgentEvent` → AG-UI event mapper; both WebSocket endpoints share a `FrameBudget` pacer that keeps outbound traffic inside AgentCore's frame-size and frame-rate quotas.

**Tech Stack:** Rust 2024 (MSRV 1.94), axum 0.8 (`json` + `ws`), tokio, `futures-util`, `serde`/`serde_json`, `uuid` (v4), `jiff` (RFC 3339 timestamps), `async-trait`, `thiserror`, `tracing`. Tests: `tokio::test`, `tower::ServiceExt::oneshot`, `tokio-tungstenite` (dev-dep), `tokio::time::pause`.

**Design doc:** `docs/superpowers/specs/2026-08-08-sma-461-agentcore-a2a-agui-design.md` — read it before starting. Section references below (§4.1, §6.2, …) point into it.

## Global Constraints

- **Worktree:** all work happens in `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-461` on branch `feature/sma-461-runtime-agentcore-a2a-and-ag-ui-protocol-shims`. Never `cd` to the main checkout. Never run `git stash` bare, `git checkout <branch>`, or anything else that moves HEAD — the object store is shared with other sessions.
- **Commit format:** `<type>(runtime-agentcore): SMA-461 <lowercase message>`. `runtime-agentcore` is in the `.versionrc` scope allowlist; `docs`/`spec`/`plan`/`ci`/`workflows` are also valid scopes for doc- and CI-only commits. A local `commit-msg` hook runs `convco check` and will reject anything else.
- **Before every commit:** run `cargo fmt --all` then `cargo clippy --workspace --all-features --all-targets -- -D warnings`. The `pre-commit` hook is a deliberate no-op, so nothing catches formatting for you until `pre-push` (which is slow). Hand-edited Rust that skips this reliably fails CI.
- **`missing_docs` is workspace-deny-on-warn.** Every `pub` item — including every field of every `pub` struct and every variant of every `pub` enum — needs a `///` doc comment, or the required `docs` job fails under `RUSTDOCFLAGS=-D warnings`. Keep anything not in a public signature `pub(crate)`.
- **Never link from a `pub` item's docs to a `pub(crate)`/private item** with intra-doc syntax (`` [`crate::thing`] ``). That trips `rustdoc::private_intra_doc_links`, which fails only the `docs` job — builds and tests stay green. Use prose instead.
- **Feature hygiene:** every new module, every `AppStateInner` field, and every test module must be `#[cfg]`-gated so `cargo build -p paigasus-helikon-runtime-agentcore --no-default-features` and `cargo test -p paigasus-helikon-runtime-agentcore --no-default-features` both succeed. This is the single most likely thing to break.
- **The `jiff` workspace pin is an exact `=0.2.28`** for a recorded reason. Do not relax it.
- **AgentCore quota constants** (§2.2, §7.1): `MAX_FRAME_BYTES = 60_000` (serialized bytes, against a conservative 64 000-byte reading of AWS's "64 KB"), `FRAME_RATE_CAP = 200` frames/sec (against AWS's 250, assuming a short sliding window). Both are `const`s with the assumption named in a comment.
- **Ports are fixed by AWS's contract and are not configurable:** HTTP/AG-UI `0.0.0.0:8080`, MCP `0.0.0.0:8000`, A2A `0.0.0.0:9000`.
- **Error codes are A2A-specification codes, never AWS's `-32051…-32055` platform table** (§5.6).

---

## File Structure

**Created:**

| File | Responsibility |
| --- | --- |
| `src/frame.rs` | `FrameBudget` — frame-rate pacing and size splitting for both `/ws` endpoints |
| `src/ws.rs` | HTTP-mode `GET /ws` handler |
| `src/agui/mod.rs` | `agui_router()`, `serve_agui()` |
| `src/agui/types.rs` | `RunAgentInput` + the AG-UI event enum (`pub(crate)`) |
| `src/agui/map.rs` | `AgentEvent` → AG-UI mapping and the bracketing state machine |
| `src/agui/sse.rs` | AG-UI `POST /invocations` |
| `src/agui/ws.rs` | AG-UI `GET /ws` |
| `src/a2a/mod.rs` | `a2a_router()`, `serve_a2a()` |
| `src/a2a/types.rs` | JSON-RPC envelope + A2A wire types |
| `src/a2a/card.rs` | Agent-card derivation and the discovery endpoint |
| `src/a2a/store.rs` | `TaskStore` trait + `InMemoryTaskStore` |
| `src/a2a/cancel.rs` | `CancelRegistry` — live-run token map |
| `src/a2a/rpc.rs` | JSON-RPC method dispatch |
| `examples/a2a_server.rs`, `examples/agui_server.rs` | Dependency-free runnable examples |

**Modified:** `Cargo.toml` (features, deps), `src/lib.rs` (module decls, re-exports, docs), `src/error.rs` (`NotFound` variant), `src/server.rs` (state fields, builder setters, `/ws` route), `README.md`, `docker/Dockerfile`, `docs/book/src/concepts/runtimes.md`, `crates/paigasus-helikon/README.md`, root `README.md`, `.github/workflows/ci.yml`.

---

### Task 1: Cargo features, `NotFound` error, module skeleton

Establishes the feature gates and empty module tree so every later task lands in a slot that already compiles both with and without default features.

**Files:**
- Modify: `crates/paigasus-helikon-runtime-agentcore/Cargo.toml`
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/error.rs`
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/lib.rs`
- Create: `crates/paigasus-helikon-runtime-agentcore/src/{frame.rs,ws.rs}`
- Create: `crates/paigasus-helikon-runtime-agentcore/src/agui/mod.rs`
- Create: `crates/paigasus-helikon-runtime-agentcore/src/a2a/mod.rs`

**Interfaces:**
- Produces: features `a2a`, `ag-ui`, `ws` (all default-on); `AgentCoreError::NotFound(String)`; module paths `crate::frame`, `crate::ws`, `crate::agui`, `crate::a2a`.

- [ ] **Step 1: Write the failing test** — append to `src/error.rs`'s existing `mod tests` (create the module if absent):

```rust
#[test]
fn not_found_maps_to_404() {
    let resp = AgentCoreError::NotFound("task nope".to_owned()).into_response();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p paigasus-helikon-runtime-agentcore not_found_maps_to_404`
Expected: FAIL — `no variant named NotFound found for enum AgentCoreError`.

- [ ] **Step 3: Add the variant**

In `src/error.rs`, add to `AgentCoreError` (the enum is already `#[non_exhaustive]`, so this is additive):

```rust
    /// The addressed resource does not exist — currently only an unknown A2A task id
    /// reaching a [`TaskStore`](crate::TaskStore) method that requires one (HTTP 404).
    #[error("not found: {0}")]
    NotFound(String),
```

Then add the arm to the existing `IntoResponse` impl's status match, next to `BadRequest`:

```rust
            AgentCoreError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
```

- [ ] **Step 4: Run it and confirm it passes**

Run: `cargo test -p paigasus-helikon-runtime-agentcore not_found_maps_to_404`
Expected: PASS.

- [ ] **Step 5: Add the features and dependencies**

In `Cargo.toml`, replace the `[features]` block and adjust `[dependencies]`/`[dev-dependencies]`:

```toml
[features]
default = ["mcp", "a2a", "ag-ui", "ws"]
# rmcp's streamable-HTTP server transport, used to expose the configured agent
# as an MCP tool (`AgentCoreServer::serve_mcp`, `src/mcp.rs`) instead of the
# AgentCore HTTP-protocol contract.
mcp               = ["dep:paigasus-helikon-mcp", "dep:rmcp", "dep:async-trait"]
# AgentCore's A2A runtime type: JSON-RPC 2.0 on port 9000 with agent-card discovery.
a2a               = ["dep:async-trait", "dep:uuid", "dep:jiff"]
# AgentCore's AG-UI runtime type: SSE + WebSocket event streams on port 8080.
ag-ui             = ["axum/ws", "dep:uuid"]
# The optional `GET /ws` endpoint on the HTTP-protocol contract.
ws                = ["axum/ws"]
# Pulls in the Anthropic provider for `examples/agent_http.rs` only; there is
# no non-example code behind it, so library consumers never need it.
example-anthropic = ["dep:paigasus-helikon-providers-anthropic"]
```

In `[dependencies]`, leave `axum` at `features = ["json"]` (the features above add `ws`) and add:

```toml
uuid                                 = { workspace = true, optional = true }
jiff                                 = { workspace = true, optional = true }
```

In `[dev-dependencies]` add:

```toml
tokio-tungstenite  = { workspace = true }
```

Add the two example entries at the end of the manifest:

```toml
[[example]]
name              = "a2a_server"
required-features = ["a2a"]

[[example]]
name              = "agui_server"
required-features = ["ag-ui"]
```

- [ ] **Step 6: Create the placeholder modules**

`src/frame.rs`:

```rust
//! [`FrameBudget`] — keeps outbound WebSocket traffic inside AgentCore's frame-size and
//! frame-rate quotas. Shared by the HTTP-protocol and AG-UI `/ws` endpoints.
```

`src/ws.rs`:

```rust
//! `GET /ws` — the optional WebSocket endpoint on AgentCore's HTTP-protocol contract.
```

`src/agui/mod.rs`:

```rust
//! AG-UI protocol mode: SSE at `/invocations` and a WebSocket at `/ws`, on port 8080.
```

`src/a2a/mod.rs`:

```rust
//! A2A protocol mode: JSON-RPC 2.0 at the root path, on port 9000.
```

In `src/lib.rs`, add the gated module declarations next to the existing ones:

```rust
#[cfg(feature = "a2a")]
mod a2a;

#[cfg(feature = "ag-ui")]
mod agui;

#[cfg(any(feature = "ws", feature = "ag-ui"))]
mod frame;

#[cfg(feature = "ws")]
mod ws;
```

- [ ] **Step 7: Verify both feature configurations build**

Run:
```bash
cargo build -p paigasus-helikon-runtime-agentcore --all-features
cargo build -p paigasus-helikon-runtime-agentcore --no-default-features
cargo test  -p paigasus-helikon-runtime-agentcore --no-default-features
```
Expected: all three succeed. (The examples do not build yet — they are created in Task 15, and their `required-features` entries reference files that must exist. If `cargo build` complains about a missing example target, create the two files now containing only `fn main() {}` and fill them in at Task 15.)

- [ ] **Step 8: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-agentcore/
git commit -m "feat(runtime-agentcore): SMA-461 add a2a, ag-ui and ws feature gates"
```

---

### Task 2: `FrameBudget` — quota pacing and size splitting

The riskiest pure-logic component. Both quotas fail *only* in deployment (AgentCore drops the connection), so this is tested exhaustively and deterministically, with no wall-clock assertions.

**Files:**
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/frame.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `pub(crate) struct FrameBudget`; `FrameBudget::new() -> Self`; `async fn admit(&mut self, frame: serde_json::Value) -> Vec<String>` returning the wire-ready text frames for one logical event, having already awaited any pacing delay; `pub(crate) const MAX_FRAME_BYTES: usize`; `pub(crate) const FRAME_RATE_CAP: u32`. Splitting behaviour is controlled by `FrameBudget::new_with_splitter(split: SplitStrategy)`, where `pub(crate) enum SplitStrategy { Content { field: &'static str }, Envelope }`.

- [ ] **Step 1: Write the failing tests**

Append to `src/frame.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn big_text(n: usize) -> String {
        "a".repeat(n)
    }

    #[tokio::test]
    async fn small_frame_passes_through_unwrapped() {
        let mut b = FrameBudget::new();
        let out = b.admit(json!({"type": "RUN_STARTED", "runId": "r1"})).await;
        assert_eq!(out.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(parsed["type"], "RUN_STARTED");
        assert!(parsed.get("seq").is_none(), "small frames must not be wrapped");
    }

    #[tokio::test]
    async fn every_emitted_frame_is_within_the_size_cap() {
        let mut b = FrameBudget::new_with_splitter(SplitStrategy::Content { field: "delta" });
        let out = b
            .admit(json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": "m0", "delta": big_text(500_000)}))
            .await;
        assert!(out.len() > 1, "an oversize frame must be split");
        for f in &out {
            assert!(
                f.len() <= MAX_FRAME_BYTES,
                "emitted frame of {} bytes exceeds MAX_FRAME_BYTES",
                f.len()
            );
        }
    }

    #[tokio::test]
    async fn content_split_preserves_the_payload_and_the_event_type() {
        let mut b = FrameBudget::new_with_splitter(SplitStrategy::Content { field: "delta" });
        let original = big_text(200_000);
        let out = b
            .admit(json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": "m0", "delta": original}))
            .await;
        let mut reassembled = String::new();
        for f in &out {
            let v: serde_json::Value = serde_json::from_str(f).unwrap();
            assert_eq!(v["type"], "TEXT_MESSAGE_CONTENT", "each split frame stays a valid event");
            assert_eq!(v["messageId"], "m0");
            reassembled.push_str(v["delta"].as_str().unwrap());
        }
        assert_eq!(reassembled, original);
    }

    /// Splitting must land on `char_indices` boundaries: a byte-offset split through a
    /// multi-byte codepoint produces invalid UTF-8 and a frame that will not parse.
    #[tokio::test]
    async fn content_split_never_lands_mid_codepoint() {
        let mut b = FrameBudget::new_with_splitter(SplitStrategy::Content { field: "delta" });
        let original = "→".repeat(100_000); // 3 bytes each
        let out = b
            .admit(json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": "m0", "delta": original}))
            .await;
        let mut reassembled = String::new();
        for f in &out {
            let v: serde_json::Value = serde_json::from_str(f).unwrap();
            reassembled.push_str(v["delta"].as_str().unwrap());
        }
        assert_eq!(reassembled, original);
    }

    /// The cap applies to serialized bytes, not payload length: JSON escaping expands
    /// control characters sixfold, so a payload comfortably under the cap can serialize
    /// well over it.
    #[tokio::test]
    async fn size_is_measured_on_serialized_bytes_not_payload_length() {
        let mut b = FrameBudget::new_with_splitter(SplitStrategy::Content { field: "delta" });
        // 20k control chars -> "" (6 bytes) each -> ~120 KB serialized.
        let payload: String = std::iter::repeat('\u{1}').take(20_000).collect();
        let out = b
            .admit(json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": "m0", "delta": payload}))
            .await;
        assert!(out.len() > 1, "escaping must be accounted for");
        for f in &out {
            assert!(f.len() <= MAX_FRAME_BYTES, "frame of {} bytes too large", f.len());
        }
    }

    #[tokio::test]
    async fn unsplittable_events_fall_back_to_the_chunk_envelope() {
        let mut b = FrameBudget::new_with_splitter(SplitStrategy::Envelope);
        let out = b
            .admit(json!({"type": "TOOL_CALL_RESULT", "content": big_text(200_000)}))
            .await;
        assert!(out.len() > 1);
        let mut reassembled = String::new();
        for (i, f) in out.iter().enumerate() {
            let v: serde_json::Value = serde_json::from_str(f).unwrap();
            assert_eq!(v["type"], "helikon.chunk");
            assert_eq!(v["seq"], i);
            assert_eq!(v["final"], i == out.len() - 1);
            reassembled.push_str(v["data"].as_str().unwrap());
        }
        let inner: serde_json::Value = serde_json::from_str(&reassembled).unwrap();
        assert_eq!(inner["type"], "TOOL_CALL_RESULT");
    }

    /// Deterministic pacing: with the clock paused, admitting more frames than the
    /// per-second cap must have awaited a total delay of at least one second. Asserting
    /// on the virtual clock (not wall time) keeps this stable across the CI matrix.
    #[tokio::test(start_paused = true)]
    async fn pacer_delays_once_the_rate_cap_is_reached() {
        let mut b = FrameBudget::new();
        let start = tokio::time::Instant::now();
        for i in 0..(FRAME_RATE_CAP + 1) {
            b.admit(json!({"type": "STEP_STARTED", "n": i})).await;
        }
        assert!(
            start.elapsed() >= std::time::Duration::from_secs(1),
            "the pacer must have slept after exceeding the cap, elapsed {:?}",
            start.elapsed()
        );
    }

    /// The pacer covers *every* frame, not just text. A burst of tool-call frames with
    /// no text involved must still be paced.
    #[tokio::test(start_paused = true)]
    async fn pacer_covers_non_text_frames() {
        let mut b = FrameBudget::new();
        let start = tokio::time::Instant::now();
        for i in 0..(FRAME_RATE_CAP + 1) {
            b.admit(json!({"type": "TOOL_CALL_RESULT", "toolCallId": format!("t{i}")}))
                .await;
        }
        assert!(start.elapsed() >= std::time::Duration::from_secs(1));
    }
}
```

- [ ] **Step 2: Run the tests and confirm they fail**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features ws frame::`
Expected: FAIL — `cannot find type FrameBudget in this scope`.

- [ ] **Step 3: Implement `FrameBudget`**

Replace the body of `src/frame.rs` (keeping the module doc comment) with:

```rust
use std::time::Duration;

use serde_json::Value;

/// Maximum serialized bytes in a single outbound WebSocket frame.
///
/// AgentCore closes the connection when a frame exceeds its documented **64 KB** limit.
/// AWS does not state whether "64 KB" means 65 536 or 64 000 bytes, so this budgets
/// against the smaller reading and leaves headroom on top of that.
pub(crate) const MAX_FRAME_BYTES: usize = 60_000;

/// Maximum frames emitted per second.
///
/// AgentCore closes the connection above **250 frames/second**. AWS does not state
/// whether that is a one-second average or a shorter sliding window, so this paces
/// against the hostile reading: a burst cannot trip a sliding window either.
pub(crate) const FRAME_RATE_CAP: u32 = 200;

/// How an oversize frame is broken up.
#[derive(Debug, Clone, Copy)]
pub(crate) enum SplitStrategy {
    /// Split one string field's value across several otherwise-identical frames. The
    /// result is N valid protocol events, so a client needs no reassembly logic.
    Content {
        /// Name of the string field to split (e.g. `"delta"`).
        field: &'static str,
    },
    /// Wrap the serialized frame in `helikon.chunk` envelopes. Used only for events
    /// whose payload cannot be split into several valid events.
    Envelope,
}

/// Paces and splits outbound WebSocket frames to stay inside AgentCore's quotas.
///
/// One instance per connection; not `Clone`, because the rate budget is per-connection.
pub(crate) struct FrameBudget {
    split: SplitStrategy,
    /// Frames emitted in the current one-second window.
    emitted: u32,
    /// Start of the current window, on the tokio clock (so `tokio::time::pause` works).
    window_start: tokio::time::Instant,
    /// Monotonic id for chunk groups, so a client can tell two interleaved groups apart.
    chunk_group: u64,
}

impl FrameBudget {
    /// A budget that wraps oversize frames in `helikon.chunk` envelopes.
    pub(crate) fn new() -> Self {
        Self::new_with_splitter(SplitStrategy::Envelope)
    }

    /// A budget using an explicit split strategy.
    pub(crate) fn new_with_splitter(split: SplitStrategy) -> Self {
        Self {
            split,
            emitted: 0,
            window_start: tokio::time::Instant::now(),
            chunk_group: 0,
        }
    }

    /// Turn one logical event into the wire-ready text frames for it, awaiting any
    /// pacing delay first.
    ///
    /// Always returns at least one frame. Every returned frame is at most
    /// [`MAX_FRAME_BYTES`] serialized bytes.
    pub(crate) async fn admit(&mut self, frame: Value) -> Vec<String> {
        let frames = self.split(frame);
        for _ in 0..frames.len() {
            self.tick().await;
        }
        frames
    }

    /// Consume one frame from the rate budget, sleeping until the next window if this
    /// window is exhausted.
    async fn tick(&mut self) {
        let elapsed = self.window_start.elapsed();
        if elapsed >= Duration::from_secs(1) {
            self.window_start = tokio::time::Instant::now();
            self.emitted = 0;
        } else if self.emitted >= FRAME_RATE_CAP {
            tokio::time::sleep(Duration::from_secs(1) - elapsed).await;
            self.window_start = tokio::time::Instant::now();
            self.emitted = 0;
        }
        self.emitted += 1;
    }

    fn split(&mut self, frame: Value) -> Vec<String> {
        let whole = frame.to_string();
        if whole.len() <= MAX_FRAME_BYTES {
            return vec![whole];
        }
        match self.split {
            SplitStrategy::Content { field } => self.split_content(frame, field),
            SplitStrategy::Envelope => self.split_envelope(&whole),
        }
    }

    /// Split `field`'s string value across several copies of the same event.
    ///
    /// Falls back to the envelope strategy when the field is absent or not a string —
    /// the frame is oversize either way and must not go out whole.
    fn split_content(&mut self, frame: Value, field: &str) -> Vec<String> {
        let Some(text) = frame.get(field).and_then(Value::as_str) else {
            return self.split_envelope(&frame.to_string());
        };
        // Budget for the envelope around the field: serialize the event with the field
        // emptied and subtract, leaving room for escaping growth inside the chunk.
        let mut probe = frame.clone();
        probe[field] = Value::String(String::new());
        let overhead = probe.to_string().len();
        // Worst case each char serializes to 6 bytes ("\uXXXX"), so budget conservatively.
        let budget = MAX_FRAME_BYTES.saturating_sub(overhead + 16) / 6;
        let budget = budget.max(1);

        let mut out = Vec::new();
        let mut chunk = String::new();
        let mut chars = 0usize;
        for c in text.chars() {
            chunk.push(c);
            chars += 1;
            if chars >= budget {
                let mut part = frame.clone();
                part[field] = Value::String(std::mem::take(&mut chunk));
                out.push(part.to_string());
                chars = 0;
            }
        }
        if !chunk.is_empty() {
            let mut part = frame.clone();
            part[field] = Value::String(chunk);
            out.push(part.to_string());
        }
        out
    }

    /// Wrap an oversize serialized frame in `helikon.chunk` envelopes.
    fn split_envelope(&mut self, whole: &str) -> Vec<String> {
        self.chunk_group += 1;
        let id = format!("c{}", self.chunk_group);
        // Envelope overhead plus worst-case 6x escaping growth inside `data`.
        let budget = (MAX_FRAME_BYTES.saturating_sub(160) / 6).max(1);

        let mut pieces: Vec<String> = Vec::new();
        let mut piece = String::new();
        let mut chars = 0usize;
        for c in whole.chars() {
            piece.push(c);
            chars += 1;
            if chars >= budget {
                pieces.push(std::mem::take(&mut piece));
                chars = 0;
            }
        }
        if !piece.is_empty() {
            pieces.push(piece);
        }

        let last = pieces.len().saturating_sub(1);
        pieces
            .into_iter()
            .enumerate()
            .map(|(seq, data)| {
                serde_json::json!({
                    "type": "helikon.chunk",
                    "id": id,
                    "seq": seq,
                    "final": seq == last,
                    "data": data,
                })
                .to_string()
            })
            .collect()
    }
}
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features ws frame::`
Expected: PASS — 8 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-agentcore/src/frame.rs
git commit -m "feat(runtime-agentcore): SMA-461 add FrameBudget websocket quota pacer"
```

---

### Task 3: HTTP-mode `GET /ws`

**Files:**
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/ws.rs`
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/server.rs` (mount the route)

**Interfaces:**
- Consumes: `crate::frame::{FrameBudget, SplitStrategy}`; `crate::server::AppState`; `crate::session::extract_session_id`; `crate::invoke::InvocationRequest` (make it `pub(crate)`-reachable — it is already `pub`).
- Produces: `pub(crate) async fn ws_upgrade<Ctx>(...)` mounted at `GET /ws` on `AgentCoreServer::router()`.

**Critical semantics (§7.2):** one `RunContext` and a **fresh `CancellationToken` per inbound run**, never per connection — `CancellationToken` is one-shot, so a reused context leaves the second run already cancelled. And an interrupt must **await the previous run's task** before starting the next, or the successor loads session history before the interrupted turn's finalize lands.

- [ ] **Step 1: Write the failing tests**

Append to `src/ws.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures_util::{
        stream::{self, BoxStream, StreamExt as _},
        SinkExt as _,
    };
    use paigasus_helikon_core::{
        Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
    };
    use tokio_tungstenite::tungstenite::Message;

    use crate::AgentCoreServer;

    /// Echoes the last user message back as an assistant message, so a test can prove
    /// the second request on a connection saw the first turn.
    struct EchoAgent;

    #[async_trait]
    impl Agent<()> for EchoAgent {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "test-only echo agent"
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            let text = input
                .messages
                .iter()
                .filter_map(|i| match i {
                    Item::UserMessage { content } => Some(
                        content
                            .iter()
                            .filter_map(|c| match c {
                                ContentPart::Text { text } => Some(text.clone()),
                                _ => None,
                            })
                            .collect::<String>(),
                    ),
                    _ => None,
                })
                .last()
                .unwrap_or_default();
            Ok(stream::iter(vec![
                AgentEvent::MessageOutput {
                    item: Item::AssistantMessage {
                        content: vec![ContentPart::Text { text }],
                        agent: Some("echo".to_owned()),
                    },
                },
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                },
            ])
            .boxed())
        }
    }

    /// Bind the HTTP-protocol router on an ephemeral port and return its `ws://` URL.
    /// WebSocket upgrades cannot be exercised through `ServiceExt::oneshot`, so these
    /// tests need a real listener.
    async fn spawn_server() -> String {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(EchoAgent))
            .with_default_context()
            .build()
            .expect("server builds");
        let router = server.router();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        format!("ws://{addr}/ws")
    }

    /// Drain frames until a terminal event, returning every frame's parsed JSON.
    async fn read_until_terminal<S>(sock: &mut S) -> Vec<serde_json::Value>
    where
        S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + Unpin,
    {
        let mut out = Vec::new();
        while let Some(Ok(msg)) = sock.next().await {
            if let Message::Text(t) = msg {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                let terminal = matches!(
                    v["type"].as_str(),
                    Some("run_completed") | Some("run_failed")
                );
                out.push(v);
                if terminal {
                    break;
                }
            }
        }
        out
    }

    #[tokio::test]
    async fn ws_runs_an_invocation_and_streams_events() {
        let url = spawn_server().await;
        let (mut sock, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        sock.send(Message::text(r#"{"prompt":"hello"}"#)).await.unwrap();
        let frames = read_until_terminal(&mut sock).await;
        assert!(
            frames.iter().any(|f| f["type"] == "run_completed"),
            "expected a terminal frame, got {frames:?}"
        );
    }

    /// Regression: `CancellationToken` is one-shot. A context built once per connection
    /// leaves the second run starting already-cancelled, so this asserts the *second*
    /// request on one connection completes too.
    #[tokio::test]
    async fn two_sequential_requests_on_one_connection_both_complete() {
        let url = spawn_server().await;
        let (mut sock, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        sock.send(Message::text(r#"{"prompt":"first"}"#)).await.unwrap();
        let first = read_until_terminal(&mut sock).await;
        assert!(first.iter().any(|f| f["type"] == "run_completed"));

        sock.send(Message::text(r#"{"prompt":"second"}"#)).await.unwrap();
        let second = read_until_terminal(&mut sock).await;
        assert!(
            second.iter().any(|f| f["type"] == "run_completed"),
            "the second run must not start already-cancelled, got {second:?}"
        );
    }

    #[tokio::test]
    async fn binary_frames_are_rejected_with_close_code_1003() {
        let url = spawn_server().await;
        let (mut sock, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        sock.send(Message::binary(vec![0u8, 1, 2])).await.unwrap();
        let mut code = None;
        while let Some(Ok(msg)) = sock.next().await {
            if let Message::Close(Some(frame)) = msg {
                code = Some(u16::from(frame.code));
                break;
            }
        }
        assert_eq!(code, Some(1003), "expected 1003 Unsupported Data");
    }

    #[tokio::test]
    async fn malformed_json_yields_an_error_frame_not_a_disconnect() {
        let url = spawn_server().await;
        let (mut sock, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        sock.send(Message::text("not json at all")).await.unwrap();
        let mut saw_error = false;
        while let Some(Ok(msg)) = sock.next().await {
            if let Message::Text(t) = msg {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                if v["type"] == "run_failed" {
                    saw_error = true;
                    break;
                }
            }
        }
        assert!(saw_error, "a bad request must surface as a run_failed frame");
    }
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features ws ws::tests`
Expected: FAIL — no `/ws` route, so `connect_async` errors with an HTTP 404.

- [ ] **Step 3: Implement the handler**

Replace `src/ws.rs`'s body (keeping the module doc) with:

```rust
use std::sync::Arc;

use axum::{
    extract::{
        ws::{CloseFrame, Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::request::Parts,
    response::Response,
};
use futures_util::StreamExt as _;
use paigasus_helikon_core::{AgentEvent, CancellationToken};
use paigasus_helikon_runtime_axum::SessionKey;

use crate::{
    error::AgentCoreError,
    frame::FrameBudget,
    invoke::InvocationRequest,
    server::AppState,
    session::extract_session_id,
};

/// Maximum bytes accepted in one inbound frame, matching `/invocations`' body cap.
const MAX_INBOUND_BYTES: usize = 2 * 1024 * 1024;

/// `GET /ws` — upgrade to a WebSocket carrying the same request vocabulary as
/// `POST /invocations`.
///
/// The session id is read from the upgrade request's headers; validation is identical
/// to `/invocations`. A rejected upgrade returns the usual contract-shaped error.
pub(crate) async fn ws_upgrade<Ctx: Send + Sync + 'static>(
    State(state): State<AppState<Ctx>>,
    upgrade: WebSocketUpgrade,
    parts_req: axum::extract::Request,
) -> Result<Response, AgentCoreError> {
    let (parts, _) = parts_req.into_parts();
    let session_id = extract_session_id(&parts.headers)?;
    Ok(upgrade
        .max_message_size(MAX_INBOUND_BYTES)
        .on_upgrade(move |socket| connection(socket, state, parts, session_id)))
}

/// Drive one upgraded connection: read a request, run it, stream its events back.
///
/// **One run at a time.** A request arriving while a run is in flight cancels the
/// in-flight run and then *awaits its task* before starting the successor — the run's
/// finalize (and therefore its session write) happens inside that task, so starting the
/// next run first would let it load history without the interrupted turn.
async fn connection<Ctx: Send + Sync + 'static>(
    socket: WebSocket,
    state: AppState<Ctx>,
    parts: Parts,
    session_id: Option<String>,
) {
    let (mut sink, mut stream) = socket.split();
    let mut budget = FrameBudget::new();
    let mut in_flight: Option<(CancellationToken, tokio::task::JoinHandle<()>)> = None;

    while let Some(Ok(msg)) = stream.next().await {
        let text = match msg {
            Message::Text(t) => t,
            Message::Binary(_) => {
                let _ = close_unsupported(&mut sink).await;
                return;
            }
            Message::Close(_) => break,
            Message::Ping(_) | Message::Pong(_) => continue,
        };

        // Interrupt: cancel, then wait for the previous run's finalize to land.
        if let Some((token, handle)) = in_flight.take() {
            token.cancel();
            let _ = handle.await;
        }

        let request: InvocationRequest = match serde_json::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                send_event(
                    &mut sink,
                    &mut budget,
                    AgentEvent::RunFailed {
                        error: format!("invalid invocation request: {e}"),
                    },
                )
                .await;
                continue;
            }
        };

        // A fresh token and a fresh RunContext per run: CancellationToken is one-shot.
        let cancel = CancellationToken::new();
        let session = match state
            .sessions
            .session(SessionKey::new(None, session_id.clone()))
            .await
        {
            Ok(s) => s,
            Err(e) => {
                send_event(
                    &mut sink,
                    &mut budget,
                    AgentEvent::RunFailed {
                        error: e.to_string(),
                    },
                )
                .await;
                continue;
            }
        };
        let ctx = match state.context.build(&parts, session, cancel.clone()).await {
            Ok(c) => c,
            Err(e) => {
                send_event(
                    &mut sink,
                    &mut budget,
                    AgentEvent::RunFailed {
                        error: e.to_string(),
                    },
                )
                .await;
                continue;
            }
        };

        let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(64);
        let runner = Arc::clone(&state.runner);
        let agent = Arc::clone(&state.agent);
        let run_config = state.run_config.clone();
        let input = request.into_agent_input();

        // Detached driver, exactly as `invoke.rs` does: the runner's finalize step only
        // runs when its stream is driven to termination, so drain unconditionally.
        let handle = tokio::spawn(async move {
            let mut events = match runner
                .run_streamed(agent.as_ref(), ctx, input, run_config)
                .await
            {
                Ok(streaming) => streaming.events,
                Err(e) => futures_util::stream::iter(vec![AgentEvent::RunFailed {
                    error: e.to_string(),
                }])
                .boxed(),
            };
            while let Some(ev) = events.next().await {
                let _ = tx.send(ev).await;
            }
        });

        while let Some(ev) = rx.recv().await {
            send_event(&mut sink, &mut budget, ev).await;
        }
        in_flight = Some((cancel, handle));
    }

    if let Some((token, handle)) = in_flight {
        token.cancel();
        let _ = handle.await;
    }
}

/// Serialize one event through the frame budget and write every resulting frame.
async fn send_event<S>(sink: &mut S, budget: &mut FrameBudget, event: AgentEvent)
where
    S: futures_util::Sink<Message> + Unpin,
{
    use futures_util::SinkExt as _;
    let Ok(value) = serde_json::to_value(&event) else {
        return;
    };
    for frame in budget.admit(value).await {
        if sink.send(Message::text(frame)).await.is_err() {
            return;
        }
    }
}

/// Close with 1003 Unsupported Data — this endpoint has no binary input model in v0.
async fn close_unsupported<S>(sink: &mut S) -> Result<(), S::Error>
where
    S: futures_util::Sink<Message> + Unpin,
{
    use futures_util::SinkExt as _;
    sink.send(Message::Close(Some(CloseFrame {
        code: 1003,
        reason: "binary frames are not supported".into(),
    })))
    .await
}
```

In `src/invoke.rs`, change `fn into_agent_input` to `pub(crate) fn into_agent_input` so this module can reuse it.

- [ ] **Step 4: Mount the route**

In `src/server.rs`, inside `AgentCoreServer::router()`, mount the endpoint behind its feature. Build the router in a `let mut` so the `#[cfg]` reads cleanly:

```rust
    pub fn router(&self) -> Router {
        #[allow(unused_mut)]
        let mut router = Router::new()
            .route("/ping", get(ping::ping))
            .route("/invocations", post(invoke::invocations::<Ctx>));

        #[cfg(feature = "ws")]
        {
            router = router.route("/ws", get(crate::ws::ws_upgrade::<Ctx>));
        }

        router.with_state(self.state.clone())
    }
```

Extend that method's doc comment with a sentence naming `/ws` and the fact that it is
gated on the `ws` feature.

- [ ] **Step 5: Run and confirm the tests pass**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features ws ws::tests`
Expected: PASS — 4 tests.

- [ ] **Step 6: Verify the feature still compiles out**

Run: `cargo build -p paigasus-helikon-runtime-agentcore --no-default-features`
Expected: success.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-agentcore/src/
git commit -m "feat(runtime-agentcore): SMA-461 add http-protocol websocket endpoint"
```

---

### Task 4: AG-UI wire types

**Files:**
- Create: `crates/paigasus-helikon-runtime-agentcore/src/agui/types.rs`
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/agui/mod.rs` (declare the module)

**Interfaces:**
- Produces: `pub(crate) struct RunAgentInput { thread_id: Option<String>, run_id: Option<String>, messages: Vec<AgUiMessage> }`; `pub(crate) struct AgUiMessage { id: Option<String>, role: String, content: Option<String> }`; `pub(crate) fn RunAgentInput::into_agent_input(self) -> AgentInput`; and an `AgUiEvent` builder module producing `serde_json::Value` frames.

Events are emitted as `serde_json::Value` rather than a typed enum: the mapper (Task 5) needs to hand frames to `FrameBudget`, which works on `Value`, and a typed enum would only be converted straight back. Constructors keep the field names in one place.

- [ ] **Step 1: Write the failing tests**

Create `src/agui/types.rs` with:

```rust
//! AG-UI wire types: the `RunAgentInput` request body and the outbound event frames.
//!
//! AWS passes request payloads to the container without validation, so unknown fields
//! (`tools`, `context`, `state`, `forwardedProps`) are accepted and ignored rather than
//! rejected — compliant AG-UI clients always send them.

#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::{ContentPart, Item};

    #[test]
    fn deserializes_the_documented_run_agent_input_shape() {
        let raw = r#"{
            "threadId": "thread-123",
            "runId": "run-456",
            "messages": [{"id": "msg-1", "role": "user", "content": "Hello, agent!"}],
            "tools": [],
            "context": [],
            "state": {},
            "forwardedProps": {}
        }"#;
        let input: RunAgentInput = serde_json::from_str(raw).unwrap();
        assert_eq!(input.thread_id.as_deref(), Some("thread-123"));
        assert_eq!(input.run_id.as_deref(), Some("run-456"));
        assert_eq!(input.messages.len(), 1);
    }

    #[test]
    fn unknown_fields_are_ignored_not_rejected() {
        let raw = r#"{"messages": [], "somethingBrandNew": {"a": 1}}"#;
        let input: RunAgentInput = serde_json::from_str(raw).unwrap();
        assert!(input.messages.is_empty());
    }

    #[test]
    fn maps_roles_onto_items() {
        let raw = r#"{"messages":[
            {"role":"user","content":"hi"},
            {"role":"assistant","content":"hello"},
            {"role":"system","content":"be nice"}
        ]}"#;
        let input: RunAgentInput = serde_json::from_str(raw).unwrap();
        let agent_input = input.into_agent_input();
        assert_eq!(agent_input.messages.len(), 3);
        assert!(matches!(agent_input.messages[0], Item::UserMessage { .. }));
        assert!(matches!(agent_input.messages[1], Item::AssistantMessage { .. }));
        assert!(matches!(agent_input.messages[2], Item::System { .. }));
        let Item::UserMessage { content } = &agent_input.messages[0] else {
            panic!("expected a user message");
        };
        assert!(matches!(&content[0], ContentPart::Text { text } if text == "hi"));
    }

    #[test]
    fn messages_without_content_are_skipped() {
        let raw = r#"{"messages":[{"role":"user"},{"role":"user","content":"real"}]}"#;
        let input: RunAgentInput = serde_json::from_str(raw).unwrap();
        assert_eq!(input.into_agent_input().messages.len(), 1);
    }

    #[test]
    fn event_constructors_use_the_documented_field_names() {
        let e = event::run_started("t1", "r1");
        assert_eq!(e["type"], "RUN_STARTED");
        assert_eq!(e["threadId"], "t1");
        assert_eq!(e["runId"], "r1");

        let e = event::text_message_content("m0", "chunk");
        assert_eq!(e["type"], "TEXT_MESSAGE_CONTENT");
        assert_eq!(e["messageId"], "m0");
        assert_eq!(e["delta"], "chunk");

        let e = event::tool_call_start("tc1", "search", "m0");
        assert_eq!(e["type"], "TOOL_CALL_START");
        assert_eq!(e["toolCallId"], "tc1");
        assert_eq!(e["toolCallName"], "search");
        assert_eq!(e["parentMessageId"], "m0");

        let e = event::custom("helikon.guardrail", serde_json::json!({"kind": "input"}));
        assert_eq!(e["type"], "CUSTOM");
        assert_eq!(e["name"], "helikon.guardrail");
        assert_eq!(e["value"]["kind"], "input");
    }
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features ag-ui agui::types`
Expected: FAIL — `cannot find type RunAgentInput`. (Add `mod types;` to `src/agui/mod.rs` first if the module is not found at all.)

- [ ] **Step 3: Implement the types**

Insert above the test module in `src/agui/types.rs`:

```rust
use paigasus_helikon_core::{AgentInput, ContentPart, Item};
use serde::Deserialize;

/// AG-UI's `RunAgentInput` request body.
///
/// Only the fields this runtime models are captured; `tools`, `context`, `state` and
/// `forwardedProps` are deliberately absent so serde ignores them (there is no
/// `deny_unknown_fields` here, by design — see the module docs).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunAgentInput {
    /// Client-supplied conversation id. Used only when the platform session header is
    /// absent, and never for persistence — AG-UI mode is stateless per request.
    #[serde(default)]
    pub(crate) thread_id: Option<String>,
    /// Client-supplied run id, echoed back in `RUN_STARTED`/`RUN_FINISHED`.
    #[serde(default)]
    pub(crate) run_id: Option<String>,
    /// The full conversation. AG-UI clients resend the entire history each request.
    #[serde(default)]
    pub(crate) messages: Vec<AgUiMessage>,
}

/// One entry in `RunAgentInput::messages`.
#[derive(Debug, Deserialize)]
pub(crate) struct AgUiMessage {
    /// Client-assigned message id. Unused by this runtime.
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) id: Option<String>,
    /// `"user"`, `"assistant"`, `"system"`, or anything else (ignored).
    pub(crate) role: String,
    /// Message text. A message without content contributes nothing.
    #[serde(default)]
    pub(crate) content: Option<String>,
}

impl RunAgentInput {
    /// Convert the whole conversation into an [`AgentInput`].
    ///
    /// AG-UI mode is stateless per request (the client owns thread state), so *every*
    /// message becomes part of the input rather than only the newest turn.
    pub(crate) fn into_agent_input(self) -> AgentInput {
        let mut input = AgentInput::new();
        input.messages = self
            .messages
            .into_iter()
            .filter_map(|m| {
                let text = m.content?;
                let content = vec![ContentPart::Text { text }];
                Some(match m.role.as_str() {
                    "assistant" => Item::AssistantMessage {
                        content,
                        agent: None,
                    },
                    "system" => Item::System { content },
                    _ => Item::UserMessage { content },
                })
            })
            .collect();
        input
    }
}

/// Constructors for the outbound AG-UI event frames.
///
/// Frames are `serde_json::Value` because they flow straight into the frame budget,
/// which works on `Value`; a typed enum would be converted right back.
pub(crate) mod event {
    use serde_json::{json, Value};

    /// `RUN_STARTED`.
    pub(crate) fn run_started(thread_id: &str, run_id: &str) -> Value {
        json!({"type": "RUN_STARTED", "threadId": thread_id, "runId": run_id})
    }

    /// `RUN_FINISHED`.
    pub(crate) fn run_finished(thread_id: &str, run_id: &str) -> Value {
        json!({"type": "RUN_FINISHED", "threadId": thread_id, "runId": run_id})
    }

    /// `RUN_ERROR`.
    pub(crate) fn run_error(code: &str, message: &str) -> Value {
        json!({"type": "RUN_ERROR", "code": code, "message": message})
    }

    /// `STEP_STARTED`.
    pub(crate) fn step_started(name: &str) -> Value {
        json!({"type": "STEP_STARTED", "stepName": name})
    }

    /// `STEP_FINISHED`.
    pub(crate) fn step_finished(name: &str) -> Value {
        json!({"type": "STEP_FINISHED", "stepName": name})
    }

    /// `TEXT_MESSAGE_START`.
    pub(crate) fn text_message_start(message_id: &str) -> Value {
        json!({"type": "TEXT_MESSAGE_START", "messageId": message_id, "role": "assistant"})
    }

    /// `TEXT_MESSAGE_CONTENT`.
    pub(crate) fn text_message_content(message_id: &str, delta: &str) -> Value {
        json!({"type": "TEXT_MESSAGE_CONTENT", "messageId": message_id, "delta": delta})
    }

    /// `TEXT_MESSAGE_END`.
    pub(crate) fn text_message_end(message_id: &str) -> Value {
        json!({"type": "TEXT_MESSAGE_END", "messageId": message_id})
    }

    /// `THINKING_TEXT_MESSAGE_START`.
    pub(crate) fn thinking_start(message_id: &str) -> Value {
        json!({"type": "THINKING_TEXT_MESSAGE_START", "messageId": message_id})
    }

    /// `THINKING_TEXT_MESSAGE_CONTENT`.
    pub(crate) fn thinking_content(message_id: &str, delta: &str) -> Value {
        json!({"type": "THINKING_TEXT_MESSAGE_CONTENT", "messageId": message_id, "delta": delta})
    }

    /// `THINKING_TEXT_MESSAGE_END`.
    pub(crate) fn thinking_end(message_id: &str) -> Value {
        json!({"type": "THINKING_TEXT_MESSAGE_END", "messageId": message_id})
    }

    /// `TOOL_CALL_START`.
    pub(crate) fn tool_call_start(call_id: &str, name: &str, parent: &str) -> Value {
        json!({
            "type": "TOOL_CALL_START",
            "toolCallId": call_id,
            "toolCallName": name,
            "parentMessageId": parent,
        })
    }

    /// `TOOL_CALL_ARGS`.
    pub(crate) fn tool_call_args(call_id: &str, delta: &str) -> Value {
        json!({"type": "TOOL_CALL_ARGS", "toolCallId": call_id, "delta": delta})
    }

    /// `TOOL_CALL_END`.
    pub(crate) fn tool_call_end(call_id: &str) -> Value {
        json!({"type": "TOOL_CALL_END", "toolCallId": call_id})
    }

    /// `TOOL_CALL_RESULT`.
    pub(crate) fn tool_call_result(call_id: &str, content: &str) -> Value {
        json!({"type": "TOOL_CALL_RESULT", "toolCallId": call_id, "content": content})
    }

    /// `CUSTOM` — the escape hatch for Helikon events AG-UI has no native type for.
    pub(crate) fn custom(name: &str, value: Value) -> Value {
        json!({"type": "CUSTOM", "name": name, "value": value})
    }
}
```

In `src/agui/mod.rs` add `pub(crate) mod types;`.

- [ ] **Step 4: Run and confirm the tests pass**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features ag-ui agui::types`
Expected: PASS — 5 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-agentcore/src/agui/
git commit -m "feat(runtime-agentcore): SMA-461 add ag-ui wire types"
```

---

### Task 5: AG-UI mapping and the bracketing state machine

The highest-correctness-risk component. Three bugs found during spec review live here: tool-call frames ordered backwards, `STEP_STARTED` never closed, and `MessageOutput`-only agents producing a blank UI.

**Files:**
- Create: `crates/paigasus-helikon-runtime-agentcore/src/agui/map.rs`
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/agui/mod.rs`

**Interfaces:**
- Consumes: `crate::agui::types::event`.
- Produces: `pub(crate) struct EventMapper`; `EventMapper::new(thread_id: String, run_id: String) -> Self`; `fn push(&mut self, event: &AgentEvent) -> Vec<Value>` — the AG-UI frames for one `AgentEvent`, with all bracketing applied; `fn finish(&mut self) -> Vec<Value>` — closes any still-open pairs (called only if the stream ends without a terminal event).

**Ordering fact this is built around (§2.3):** `ToolCallDelta` is emitted *while the model stream drains*; the matching `ToolCallItem` only later, from `transition()`. So `TOOL_CALL_START` is derived from the **first delta for a `call_id`** (whose `name` is `Some` only on that first delta), never from `ToolCallItem`.

- [ ] **Step 1: Write the failing tests**

Create `src/agui/map.rs` containing only this test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::{ContentPart, GuardrailKind, Item, TokenUsage};

    fn mapper() -> EventMapper {
        EventMapper::new("t1".to_owned(), "r1".to_owned())
    }

    /// Collect the `type` of every frame produced for a sequence of events.
    fn types(events: &[AgentEvent]) -> Vec<String> {
        let mut m = mapper();
        let mut out = Vec::new();
        for e in events {
            for f in m.push(e) {
                out.push(f["type"].as_str().unwrap().to_owned());
            }
        }
        for f in m.finish() {
            out.push(f["type"].as_str().unwrap().to_owned());
        }
        out
    }

    fn assistant(text: &str) -> Item {
        Item::AssistantMessage {
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
            agent: None,
        }
    }

    #[test]
    fn run_lifecycle_maps_to_run_started_and_finished() {
        let t = types(&[
            AgentEvent::RunStarted {
                agent: "a".to_owned(),
            },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ]);
        assert_eq!(t, vec!["RUN_STARTED", "RUN_FINISHED"]);
    }

    #[test]
    fn token_deltas_are_bracketed_exactly_once() {
        let t = types(&[
            AgentEvent::RunStarted { agent: "a".to_owned() },
            AgentEvent::TokenDelta { text: "he".to_owned() },
            AgentEvent::TokenDelta { text: "llo".to_owned() },
            AgentEvent::RunCompleted { usage: TokenUsage::default() },
        ]);
        assert_eq!(
            t,
            vec![
                "RUN_STARTED",
                "TEXT_MESSAGE_START",
                "TEXT_MESSAGE_CONTENT",
                "TEXT_MESSAGE_CONTENT",
                "TEXT_MESSAGE_END",
                "RUN_FINISHED",
            ]
        );
    }

    /// Regression for the ordering bug: `TOOL_CALL_START` must precede every
    /// `TOOL_CALL_ARGS` for the same id, even though `ToolCallItem` arrives *after* the
    /// deltas in the real core event order.
    #[test]
    fn tool_call_start_precedes_args_despite_item_arriving_last() {
        let t = types(&[
            AgentEvent::ToolCallDelta {
                call_id: "tc1".to_owned(),
                name: Some("search".to_owned()),
                args_delta: "{\"q\":".to_owned(),
            },
            AgentEvent::ToolCallDelta {
                call_id: "tc1".to_owned(),
                name: None,
                args_delta: "\"x\"}".to_owned(),
            },
            AgentEvent::ToolCallItem {
                item: Item::ToolCall {
                    call_id: "tc1".to_owned(),
                    name: "search".to_owned(),
                    args: serde_json::json!({"q": "x"}),
                },
            },
        ]);
        assert_eq!(
            t,
            vec![
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END",
            ]
        );
        let start = t.iter().position(|x| x == "TOOL_CALL_START").unwrap();
        let first_args = t.iter().position(|x| x == "TOOL_CALL_ARGS").unwrap();
        assert!(start < first_args, "START must precede ARGS");
    }

    /// A non-streaming provider emits no deltas at all: `ToolCallItem` must then
    /// synthesize the whole triple rather than emit a bare, unmatched END.
    #[test]
    fn tool_call_item_without_deltas_synthesizes_the_full_triple() {
        let t = types(&[AgentEvent::ToolCallItem {
            item: Item::ToolCall {
                call_id: "tc9".to_owned(),
                name: "lookup".to_owned(),
                args: serde_json::json!({"a": 1}),
            },
        }]);
        assert_eq!(t, vec!["TOOL_CALL_START", "TOOL_CALL_ARGS", "TOOL_CALL_END"]);
    }

    /// Regression: an agent that emits only `MessageOutput` (non-streaming providers,
    /// workflow agents, the crate's own test fixtures) must still produce *visible*
    /// text. Asserting balance alone would pass on an empty stream.
    #[test]
    fn message_output_without_deltas_produces_visible_text() {
        let mut m = mapper();
        let frames = m.push(&AgentEvent::MessageOutput {
            item: assistant("the whole answer"),
        });
        let kinds: Vec<&str> = frames.iter().map(|f| f["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            vec!["TEXT_MESSAGE_START", "TEXT_MESSAGE_CONTENT", "TEXT_MESSAGE_END"]
        );
        assert_eq!(frames[1]["delta"], "the whole answer");
    }

    /// When deltas already streamed the text, `MessageOutput` only closes the run — it
    /// must not repeat the text.
    #[test]
    fn message_output_after_deltas_only_closes_the_run() {
        let mut m = mapper();
        let _ = m.push(&AgentEvent::TokenDelta {
            text: "streamed".to_owned(),
        });
        let frames = m.push(&AgentEvent::MessageOutput {
            item: assistant("streamed"),
        });
        let kinds: Vec<&str> = frames.iter().map(|f| f["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["TEXT_MESSAGE_END"]);
    }

    /// Regression: `STEP_STARTED` is a paired event with no "turn finished" source
    /// event, so the mapper must close it on the next turn and on the terminal.
    #[test]
    fn steps_are_balanced_across_turns() {
        let t = types(&[
            AgentEvent::TurnStarted { turn: 0 },
            AgentEvent::TurnStarted { turn: 1 },
            AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            },
        ]);
        assert_eq!(
            t,
            vec![
                "STEP_STARTED",
                "STEP_FINISHED",
                "STEP_STARTED",
                "STEP_FINISHED",
                "RUN_FINISHED",
            ]
        );
    }

    /// Every opened pair must close even when the run fails mid-text.
    #[test]
    fn run_failed_mid_text_closes_every_open_pair() {
        let t = types(&[
            AgentEvent::TurnStarted { turn: 0 },
            AgentEvent::TokenDelta { text: "partial".to_owned() },
            AgentEvent::ReasoningDelta { text: "hmm".to_owned() },
            AgentEvent::RunFailed { error: "boom".to_owned() },
        ]);
        assert_eq!(
            t,
            vec![
                "STEP_STARTED",
                "TEXT_MESSAGE_START",
                "TEXT_MESSAGE_CONTENT",
                "TEXT_MESSAGE_END",
                "THINKING_TEXT_MESSAGE_START",
                "THINKING_TEXT_MESSAGE_CONTENT",
                "THINKING_TEXT_MESSAGE_END",
                "STEP_FINISHED",
                "RUN_ERROR",
            ]
        );
    }

    #[test]
    fn interleaved_text_and_reasoning_never_overlap() {
        let t = types(&[
            AgentEvent::TokenDelta { text: "a".to_owned() },
            AgentEvent::ReasoningDelta { text: "b".to_owned() },
            AgentEvent::TokenDelta { text: "c".to_owned() },
            AgentEvent::RunCompleted { usage: TokenUsage::default() },
        ]);
        assert_eq!(
            t,
            vec![
                "TEXT_MESSAGE_START",
                "TEXT_MESSAGE_CONTENT",
                "TEXT_MESSAGE_END",
                "THINKING_TEXT_MESSAGE_START",
                "THINKING_TEXT_MESSAGE_CONTENT",
                "THINKING_TEXT_MESSAGE_END",
                "TEXT_MESSAGE_START",
                "TEXT_MESSAGE_CONTENT",
                "TEXT_MESSAGE_END",
                "RUN_FINISHED",
            ]
        );
    }

    #[test]
    fn tool_output_maps_to_tool_call_result() {
        let t = types(&[AgentEvent::ToolOutputItem {
            item: Item::ToolResult {
                call_id: "tc1".to_owned(),
                content: vec![ContentPart::Text {
                    text: "done".to_owned(),
                }],
            },
        }]);
        assert_eq!(t, vec!["TOOL_CALL_RESULT"]);
    }

    #[test]
    fn helikon_specific_events_become_namespaced_custom_events() {
        let cases: Vec<(AgentEvent, &str)> = vec![
            (
                AgentEvent::GuardrailTriggered {
                    kind: GuardrailKind::Input,
                    info: serde_json::json!({}),
                },
                "helikon.guardrail",
            ),
            (
                AgentEvent::ApprovalRequested {
                    call_id: "c".to_owned(),
                    tool: "t".to_owned(),
                    args: serde_json::json!({}),
                },
                "helikon.approval",
            ),
            (
                AgentEvent::PermissionDenied {
                    tool: "t".to_owned(),
                    reason: "nope".to_owned(),
                },
                "helikon.permission_denied",
            ),
            (
                AgentEvent::HandoffItem {
                    from: "a".to_owned(),
                    to: "b".to_owned(),
                },
                "helikon.handoff",
            ),
            (
                AgentEvent::AgentUpdated {
                    agent: "b".to_owned(),
                },
                "helikon.agent_updated",
            ),
            (AgentEvent::RepairStarted { attempt: 1 }, "helikon.repair"),
            (
                AgentEvent::StructuredOutputFailed {
                    schema_errors: vec!["e".to_owned()],
                    final_text: "x".to_owned(),
                },
                "helikon.structured_output_failed",
            ),
        ];
        for (event, expected_name) in cases {
            let mut m = mapper();
            let frames = m.push(&event);
            assert_eq!(frames.len(), 1, "expected one frame for {expected_name}");
            assert_eq!(frames[0]["type"], "CUSTOM");
            assert_eq!(frames[0]["name"], expected_name);
            assert!(
                frames[0]["value"].is_object(),
                "the original event JSON must be carried"
            );
        }
    }

    /// `AgentEvent` is `#[non_exhaustive]`, so the mapper's `match` needs a wildcard.
    /// This asserts every variant maps to at least one frame, making the count visible
    /// in review even though the compiler cannot enforce exhaustiveness.
    #[test]
    fn every_known_variant_maps_to_at_least_one_frame() {
        let all: Vec<AgentEvent> = vec![
            AgentEvent::RunStarted { agent: "a".to_owned() },
            AgentEvent::TurnStarted { turn: 0 },
            AgentEvent::TokenDelta { text: "t".to_owned() },
            AgentEvent::ReasoningDelta { text: "r".to_owned() },
            AgentEvent::ToolCallDelta {
                call_id: "c".to_owned(),
                name: Some("n".to_owned()),
                args_delta: "{}".to_owned(),
            },
            AgentEvent::MessageOutput { item: assistant("m") },
            AgentEvent::ToolCallItem {
                item: Item::ToolCall {
                    call_id: "c2".to_owned(),
                    name: "n".to_owned(),
                    args: serde_json::json!({}),
                },
            },
            AgentEvent::ToolOutputItem {
                item: Item::ToolResult {
                    call_id: "c2".to_owned(),
                    content: vec![ContentPart::Text { text: "o".to_owned() }],
                },
            },
            AgentEvent::HandoffItem { from: "a".to_owned(), to: "b".to_owned() },
            AgentEvent::AgentUpdated { agent: "b".to_owned() },
            AgentEvent::GuardrailTriggered {
                kind: GuardrailKind::Input,
                info: serde_json::json!({}),
            },
            AgentEvent::ApprovalRequested {
                call_id: "c".to_owned(),
                tool: "t".to_owned(),
                args: serde_json::json!({}),
            },
            AgentEvent::PermissionDenied { tool: "t".to_owned(), reason: "r".to_owned() },
            AgentEvent::RepairStarted { attempt: 1 },
            AgentEvent::StructuredOutputFailed {
                schema_errors: vec![],
                final_text: String::new(),
            },
            AgentEvent::RunCompleted { usage: TokenUsage::default() },
            AgentEvent::RunFailed { error: "e".to_owned() },
        ];
        assert_eq!(all.len(), 17, "AgentEvent gained or lost a variant — update the mapper");
        for event in &all {
            let mut m = mapper();
            assert!(
                !m.push(event).is_empty(),
                "no frame produced for {event:?} — the wildcard arm must not drop events"
            );
        }
    }
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features ag-ui agui::map`
Expected: FAIL — `cannot find type EventMapper`. (Add `pub(crate) mod map;` to `src/agui/mod.rs`.)

- [ ] **Step 3: Implement the mapper**

Insert above the test module in `src/agui/map.rs`:

```rust
//! Maps [`AgentEvent`]s onto AG-UI event frames, with bracketing.
//!
//! # Why bracketing lives here
//!
//! `TokenDelta`/`ReasoningDelta` are bare fragments, but AG-UI requires balanced
//! `*_START` … `*_CONTENT` … `*_END` triples, and `STEP_STARTED` has no
//! "turn finished" event to close it. [`EventMapper`] owns those pairings so no
//! transport has to.
//!
//! # Ordering
//!
//! `ToolCallDelta` is emitted while the model stream drains; the matching
//! `ToolCallItem` only afterwards. `TOOL_CALL_START` is therefore derived from the
//! *first delta* for a call id — its `name` is populated only on that first delta,
//! which is exactly the START payload — and never from `ToolCallItem`, which would
//! put START after the ARGS frames it must precede.

use std::collections::HashSet;

use paigasus_helikon_core::{AgentEvent, ContentPart, Item};
use serde_json::Value;

use crate::agui::types::event;

/// Which text-like pair is currently open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenText {
    None,
    Message,
    Thinking,
}

/// Stateful `AgentEvent` → AG-UI frame mapper for exactly one run.
pub(crate) struct EventMapper {
    thread_id: String,
    run_id: String,
    open_text: OpenText,
    /// Id of the currently-open text or thinking message.
    current_message: String,
    /// Monotonic counter behind `msg-N` ids. Stream-local uniqueness is all AG-UI
    /// requires, and deterministic ids let tests assert exact frame sequences.
    next_message: u32,
    /// Call ids that have had a `TOOL_CALL_START` emitted.
    started_calls: HashSet<String>,
    /// Whether a `STEP_STARTED` is currently unmatched.
    step_open: bool,
}

impl EventMapper {
    /// Create a mapper for one run.
    pub(crate) fn new(thread_id: String, run_id: String) -> Self {
        Self {
            thread_id,
            run_id,
            open_text: OpenText::None,
            current_message: String::new(),
            next_message: 0,
            started_calls: HashSet::new(),
            step_open: false,
        }
    }

    /// Map one event, emitting any bracketing frames it implies.
    pub(crate) fn push(&mut self, ev: &AgentEvent) -> Vec<Value> {
        let mut out = Vec::new();
        match ev {
            AgentEvent::RunStarted { .. } => {
                out.push(event::run_started(&self.thread_id, &self.run_id));
            }
            AgentEvent::TurnStarted { .. } => {
                self.close_text(&mut out);
                self.close_step(&mut out);
                self.step_open = true;
                out.push(event::step_started("turn"));
            }
            AgentEvent::TokenDelta { text } => {
                self.open_text(OpenText::Message, &mut out);
                out.push(event::text_message_content(&self.current_message, text));
            }
            AgentEvent::ReasoningDelta { text } => {
                self.open_text(OpenText::Thinking, &mut out);
                out.push(event::thinking_content(&self.current_message, text));
            }
            AgentEvent::ToolCallDelta {
                call_id,
                name,
                args_delta,
            } => {
                self.close_text(&mut out);
                if self.started_calls.insert(call_id.clone()) {
                    out.push(event::tool_call_start(
                        call_id,
                        name.as_deref().unwrap_or("unknown"),
                        &self.current_message,
                    ));
                }
                out.push(event::tool_call_args(call_id, args_delta));
            }
            AgentEvent::ToolCallItem { item } => {
                self.close_text(&mut out);
                if let Item::ToolCall {
                    call_id,
                    name,
                    args,
                } = item
                {
                    // No deltas streamed for this call (non-streaming provider):
                    // synthesize the whole triple so the client sees a complete call.
                    if self.started_calls.insert(call_id.clone()) {
                        out.push(event::tool_call_start(call_id, name, &self.current_message));
                        out.push(event::tool_call_args(call_id, &args.to_string()));
                    }
                    out.push(event::tool_call_end(call_id));
                } else {
                    out.push(event::custom("helikon.unknown", to_value(ev)));
                }
            }
            AgentEvent::ToolOutputItem { item } => {
                self.close_text(&mut out);
                if let Item::ToolResult { call_id, content } = item {
                    out.push(event::tool_call_result(call_id, &text_of(content)));
                } else {
                    out.push(event::custom("helikon.unknown", to_value(ev)));
                }
            }
            AgentEvent::MessageOutput { item } => {
                if self.open_text == OpenText::Message {
                    // Deltas already streamed this text; only close it.
                    self.close_text(&mut out);
                } else {
                    // No deltas were emitted (non-streaming provider, workflow agent):
                    // synthesize the full triple, or the client renders nothing at all.
                    self.close_text(&mut out);
                    let content = match item {
                        Item::AssistantMessage { content, .. } => text_of(content),
                        _ => String::new(),
                    };
                    let id = self.new_message_id();
                    out.push(event::text_message_start(&id));
                    out.push(event::text_message_content(&id, &content));
                    out.push(event::text_message_end(&id));
                }
            }
            AgentEvent::HandoffItem { .. } => out.push(self.custom("helikon.handoff", ev)),
            AgentEvent::AgentUpdated { .. } => out.push(self.custom("helikon.agent_updated", ev)),
            AgentEvent::GuardrailTriggered { .. } => out.push(self.custom("helikon.guardrail", ev)),
            AgentEvent::ApprovalRequested { .. } => out.push(self.custom("helikon.approval", ev)),
            AgentEvent::PermissionDenied { .. } => {
                out.push(self.custom("helikon.permission_denied", ev));
            }
            AgentEvent::RepairStarted { .. } => out.push(self.custom("helikon.repair", ev)),
            AgentEvent::StructuredOutputFailed { .. } => {
                out.push(self.custom("helikon.structured_output_failed", ev));
            }
            AgentEvent::RunCompleted { .. } => {
                self.close_all(&mut out);
                out.push(event::run_finished(&self.thread_id, &self.run_id));
            }
            AgentEvent::RunFailed { error } => {
                self.close_all(&mut out);
                out.push(event::run_error("AGENT_ERROR", error));
            }
            // `AgentEvent` is `#[non_exhaustive]`: a variant added to core later must
            // degrade to a lossless CUSTOM event rather than vanish.
            other => out.push(self.custom("helikon.unknown", other)),
        }
        out
    }

    /// Close any pairs still open. Only needed when a stream ends without a terminal
    /// event; the terminal arms already call [`EventMapper::close_all`].
    pub(crate) fn finish(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        self.close_all(&mut out);
        out
    }

    fn custom(&self, name: &str, ev: &AgentEvent) -> Value {
        event::custom(name, to_value(ev))
    }

    fn new_message_id(&mut self) -> String {
        let id = format!("msg-{}", self.next_message);
        self.next_message += 1;
        id
    }

    fn open_text(&mut self, kind: OpenText, out: &mut Vec<Value>) {
        if self.open_text == kind {
            return;
        }
        self.close_text(out);
        let id = self.new_message_id();
        self.current_message = id.clone();
        match kind {
            OpenText::Message => out.push(event::text_message_start(&id)),
            OpenText::Thinking => out.push(event::thinking_start(&id)),
            OpenText::None => return,
        }
        self.open_text = kind;
    }

    fn close_text(&mut self, out: &mut Vec<Value>) {
        match self.open_text {
            OpenText::Message => out.push(event::text_message_end(&self.current_message)),
            OpenText::Thinking => out.push(event::thinking_end(&self.current_message)),
            OpenText::None => {}
        }
        self.open_text = OpenText::None;
    }

    fn close_step(&mut self, out: &mut Vec<Value>) {
        if self.step_open {
            out.push(event::step_finished("turn"));
            self.step_open = false;
        }
    }

    fn close_all(&mut self, out: &mut Vec<Value>) {
        self.close_text(out);
        self.close_step(out);
    }
}

/// Serialize an event for a `CUSTOM` frame's `value`, degrading to `null` rather than
/// failing — a frame with a null value still tells the client the event happened.
fn to_value(ev: &AgentEvent) -> Value {
    serde_json::to_value(ev).unwrap_or(Value::Null)
}

/// Concatenate the text blocks of a content list, ignoring non-text parts.
fn text_of(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|c| match c {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}
```

- [ ] **Step 4: Run and confirm the tests pass**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features ag-ui agui::map`
Expected: PASS — 13 tests. If `every_known_variant_maps_to_at_least_one_frame` fails on the `assert_eq!(all.len(), 17, …)` line, `AgentEvent` changed upstream: add the new variant to the list *and* give it an explicit arm in `push`.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-agentcore/src/agui/map.rs crates/paigasus-helikon-runtime-agentcore/src/agui/mod.rs
git commit -m "feat(runtime-agentcore): SMA-461 add ag-ui event mapper with bracketing"
```

---

### Task 6: AG-UI `POST /invocations` (SSE) and `serve_agui()`

**Files:**
- Create: `crates/paigasus-helikon-runtime-agentcore/src/agui/sse.rs`
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/agui/mod.rs`

**Interfaces:**
- Consumes: `EventMapper`, `RunAgentInput`, `AppState`, `extract_session_id`, `PingState`.
- Produces: `impl<Ctx> AgentCoreServer<Ctx> { pub fn agui_router(&self) -> Router; pub async fn serve_agui(self) -> Result<(), AgentCoreError>; }`.

**Critical semantics (§6.1):** AG-UI mode is **stateless per request** — a fresh, unshared `InMemorySessionProvider` session per request, with `RunAgentInput.messages` as the whole conversation. Combining a persisted session with the client's full history double-counts every prior turn, because `Runner::run` seeds `history ++ input.messages`.

- [ ] **Step 1: Write the failing tests**

Create `src/agui/sse.rs` with this test module:

```rust
#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use futures_util::stream::{self, BoxStream, StreamExt as _};
    use paigasus_helikon_core::{
        Agent, AgentError, AgentEvent, AgentInput, ContentPart, Item, RunContext, TokenUsage,
    };
    use tower::ServiceExt as _;

    use crate::AgentCoreServer;

    /// Records how many messages each run was given, so a test can prove turn 2 was not
    /// handed the conversation twice.
    struct CountingAgent {
        seen: Arc<Mutex<Vec<usize>>>,
    }

    #[async_trait]
    impl Agent<()> for CountingAgent {
        fn name(&self) -> &str {
            "counting"
        }
        fn description(&self) -> &str {
            "records input message counts"
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            self.seen.lock().unwrap().push(input.messages.len());
            Ok(stream::iter(vec![
                AgentEvent::MessageOutput {
                    item: Item::AssistantMessage {
                        content: vec![ContentPart::Text {
                            text: "ok".to_owned(),
                        }],
                        agent: None,
                    },
                },
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                },
            ])
            .boxed())
        }
    }

    fn server(seen: Arc<Mutex<Vec<usize>>>) -> AgentCoreServer<()> {
        AgentCoreServer::builder()
            .agent(Arc::new(CountingAgent { seen }))
            .with_default_context()
            .build()
            .expect("server builds")
    }

    async fn post(server: &AgentCoreServer<()>, body: &str, session: Option<&str>) -> String {
        let mut req = Request::builder().method("POST").uri("/invocations");
        if let Some(s) = session {
            req = req.header("X-Amzn-Bedrock-AgentCore-Runtime-Session-Id", s);
        }
        let resp = server
            .agui_router()
            .oneshot(req.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn streams_the_documented_agui_event_sequence() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let body = post(
            &server(Arc::clone(&seen)),
            r#"{"threadId":"t1","runId":"r1","messages":[{"role":"user","content":"hi"}]}"#,
            None,
        )
        .await;
        assert!(body.contains(r#""type":"RUN_STARTED""#), "body: {body}");
        assert!(body.contains(r#""type":"TEXT_MESSAGE_START""#), "body: {body}");
        assert!(body.contains(r#""type":"RUN_FINISHED""#), "body: {body}");
        assert!(body.contains(r#""threadId":"t1""#));
        assert!(body.contains(r#""runId":"r1""#));
    }

    /// Regression for the double-counting bug: AG-UI clients resend the full
    /// conversation each turn, so a second request carrying 3 messages must reach the
    /// agent as exactly 3 — not 3 plus a replayed session history.
    #[tokio::test]
    async fn turn_two_does_not_double_count_history() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = server(Arc::clone(&seen));
        let session = "a-session-id-that-is-long-enough-to-pass-validation-000";

        post(
            &s,
            r#"{"threadId":"t1","runId":"r1","messages":[{"role":"user","content":"one"}]}"#,
            Some(session),
        )
        .await;
        post(
            &s,
            r#"{"threadId":"t1","runId":"r2","messages":[
                {"role":"user","content":"one"},
                {"role":"assistant","content":"ok"},
                {"role":"user","content":"two"}
            ]}"#,
            Some(session),
        )
        .await;

        let counts = seen.lock().unwrap().clone();
        assert_eq!(
            counts,
            vec![1, 3],
            "turn 2 must see exactly the client's 3 messages, not a doubled history"
        );
    }

    #[tokio::test]
    async fn an_invalid_body_yields_a_run_error_frame() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = server(seen);
        let resp = s
            .agui_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/invocations")
                    .body(Body::from("not json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains(r#""type":"RUN_ERROR""#), "body: {body}");
        assert!(body.contains("VALIDATION_ERROR"), "body: {body}");
    }

    #[tokio::test]
    async fn ping_is_reachable_on_the_agui_router() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let resp = server(seen)
            .agui_router()
            .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features ag-ui agui::sse`
Expected: FAIL — `no method named agui_router`.

- [ ] **Step 3: Implement the handler**

Insert above the tests in `src/agui/sse.rs`:

```rust
//! AG-UI `POST /invocations` — `RunAgentInput` in, an AG-UI SSE event stream out.
//!
//! # Stateless per request
//!
//! AG-UI clients resend the entire conversation in `messages` on every request, while
//! `Runner::run` seeds the model with `history ++ input.messages`. Pairing a persisted
//! session with a full client history therefore double-counts every prior turn. This
//! handler resolves a **fresh, unshared session per request** and treats `messages` as
//! the whole conversation — the same shape MCP mode uses, and with the same consequence:
//! AG-UI mode cannot use a persistent session backend in v0.
//!
//! # Disconnect
//!
//! Identical to `/invocations`': the run is driven by a detached task so its finalize
//! step always runs, with a `CancellationToken` drop-guard on the response so a departed
//! client stops the run. Unlike A2A, the guard *does* apply here — AG-UI has no
//! resubscribe, so nothing is waiting to reattach.

use std::{convert::Infallible, sync::Arc};

use axum::{
    extract::{Request, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Response, Sse,
    },
};
use futures_util::{stream, StreamExt as _};
use paigasus_helikon_core::{AgentEvent, CancellationToken, Session};
use paigasus_helikon_runtime_axum::{InMemorySessionProvider, SessionKey, SessionProvider as _};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{
    agui::{map::EventMapper, types::RunAgentInput},
    server::AppState,
    session::extract_session_id,
};

/// Upper bound on the buffered request body (2 MiB), matching `/invocations`.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// `POST /invocations` — see the module docs for the full contract.
pub(crate) async fn invocations<Ctx: Send + Sync + 'static>(
    State(state): State<AppState<Ctx>>,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();

    // Validate the session header for its isolation value even though AG-UI mode does
    // not persist through it — a malformed header is still a contract violation.
    let header_session = match extract_session_id(&parts.headers) {
        Ok(id) => id,
        Err(e) => return error_stream(StatusCode::BAD_REQUEST, "VALIDATION_ERROR", &e.to_string()),
    };

    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            return error_stream(
                StatusCode::BAD_REQUEST,
                "VALIDATION_ERROR",
                &format!("failed to read request body: {e}"),
            )
        }
    };
    let input: RunAgentInput = match serde_json::from_slice(&bytes) {
        Ok(i) => i,
        Err(e) => {
            return error_stream(
                StatusCode::BAD_REQUEST,
                "VALIDATION_ERROR",
                &format!("invalid RunAgentInput body: {e}"),
            )
        }
    };

    let thread_id = header_session
        .clone()
        .or_else(|| input.thread_id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let run_id = input
        .run_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    // Fresh, unshared session: see the module docs.
    let session: Arc<dyn Session> = match InMemorySessionProvider::new(1)
        .session(SessionKey::new(None, None))
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return error_stream(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                &e.to_string(),
            )
        }
    };

    let cancel = CancellationToken::new();
    let cancel_for_run = cancel.clone();
    let ctx = match state.context.build(&parts, session, cancel).await {
        Ok(c) => c,
        Err(e) => {
            return error_stream(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                &e.to_string(),
            )
        }
    };

    let agent_input = input.into_agent_input();
    let (tx, rx) = mpsc::channel::<AgentEvent>(64);
    let runner = Arc::clone(&state.runner);
    let agent = Arc::clone(&state.agent);
    let run_config = state.run_config.clone();

    // Detached driver: drains unconditionally so the runner's finalize step always runs.
    tokio::spawn(async move {
        let mut events = match runner
            .run_streamed(agent.as_ref(), ctx, agent_input, run_config)
            .await
        {
            Ok(streaming) => streaming.events,
            Err(e) => stream::iter(vec![AgentEvent::RunFailed {
                error: e.to_string(),
            }])
            .boxed(),
        };
        while let Some(ev) = events.next().await {
            let _ = tx.send(ev).await;
        }
    });

    let mut mapper = EventMapper::new(thread_id, run_id);
    let guard = cancel_for_run.drop_guard();
    let frames = tokio_stream::wrappers::ReceiverStream::new(rx)
        .flat_map(move |ev| stream::iter(mapper.push(&ev)))
        .map(move |value| {
            // Touch the guard so it lives as long as the response stream: dropping it
            // early would cancel every run the instant it started.
            let _ = &guard;
            Ok::<_, Infallible>(Event::default().data(value.to_string()))
        });

    Sse::new(frames).keep_alive(KeepAlive::default()).into_response()
}

/// A single-frame `RUN_ERROR` stream with a real HTTP status.
///
/// AG-UI serializes every error as a `RUN_ERROR` SSE event; the status code is the
/// error's own when the stream has not begun, and `200` once it has (which is the
/// `AGENT_ERROR` case handled inside the stream instead).
fn error_stream(status: StatusCode, code: &str, message: &str) -> Response {
    let frame = crate::agui::types::event::run_error(code, message);
    let body = format!("data: {frame}\n\n");
    (
        status,
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
        .into_response()
}
```

Note the `tokio_stream` usage — add `tokio-stream = { workspace = true }` to `[dependencies]` if the workspace pins it; otherwise replace `ReceiverStream::new(rx)` with `futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|ev| (ev, rx)) })` and drop the dependency.

- [ ] **Step 4: Add the router and serve entry points**

Replace `src/agui/mod.rs` with:

```rust
//! AG-UI protocol mode: SSE at `/invocations` and a WebSocket at `/ws`, on port 8080.
//!
//! AG-UI and the HTTP protocol are alternative `serverProtocol` settings for one
//! AgentCore container, so a deployment runs one or the other. They share port 8080 and
//! the `/invocations` path: a container configured `serverProtocol: HTTP` that calls
//! [`AgentCoreServer::serve_agui`] will serve traffic successfully but with the wrong
//! event vocabulary. Pick the mode that matches the runtime's configured protocol.

pub(crate) mod map;
pub(crate) mod sse;
pub(crate) mod types;
pub(crate) mod ws;

use axum::{
    routing::{get, post},
    Router,
};

use crate::{error::AgentCoreError, ping, server::AgentCoreServer};

/// Fixed bind address for AG-UI mode. The same port as the HTTP protocol, per AWS's
/// contract; the two are alternative protocols for one container, not concurrent ones.
const AGUI_ADDR: &str = "0.0.0.0:8080";

impl<Ctx: Send + Sync + 'static> AgentCoreServer<Ctx> {
    /// Build the AG-UI router: `POST /invocations` (SSE), `GET /ws`, and `GET /ping`.
    ///
    /// Pure: spawns nothing. Suitable for embedding or for testing with `tower`'s
    /// `ServiceExt::oneshot` (WebSocket upgrades excepted — those need a real listener).
    ///
    /// This router's `/invocations` and `/ping` paths collide with
    /// [`AgentCoreServer::router`]'s. `Router::merge` panics on overlapping routes, so
    /// merge one or the other into a larger app, never both.
    ///
    /// AG-UI's `/invocations` is **SSE only** — it does not honour
    /// `Accept: application/json`, because the AG-UI contract defines no buffered form.
    pub fn agui_router(&self) -> Router {
        Router::new()
            .route("/ping", get(ping::ping))
            .route("/invocations", post(sse::invocations::<Ctx>))
            .route("/ws", get(ws::ws_upgrade::<Ctx>))
            .with_state(self.state_for_agui())
    }

    /// Serve the configured agent over AG-UI: binds `0.0.0.0:8080` and serves
    /// [`AgentCoreServer::agui_router`] until the process is terminated.
    ///
    /// Logs `"ready in {ms}ms"` immediately after the listener is bound, exactly like
    /// [`AgentCoreServer::serve`].
    ///
    /// # Errors
    ///
    /// Returns [`AgentCoreError::Internal`] if binding the listener or the serve loop
    /// fails. A bind failure here most often means another mode is already on 8080 —
    /// AG-UI and the HTTP protocol cannot both run in one container.
    pub async fn serve_agui(self) -> Result<(), AgentCoreError> {
        let start = std::time::Instant::now();
        let router = self.agui_router();
        let listener = tokio::net::TcpListener::bind(AGUI_ADDR).await.map_err(|e| {
            AgentCoreError::Internal(format!(
                "failed to bind {AGUI_ADDR}: {e} \
                 (AG-UI and the HTTP protocol both use 8080; run only one per container)"
            ))
        })?;
        let elapsed_ms = start.elapsed().as_millis();
        tracing::info!(elapsed_ms = elapsed_ms as u64, "ready in {elapsed_ms}ms");
        axum::serve(listener, router)
            .await
            .map_err(|e| AgentCoreError::Internal(e.to_string()))
    }
}
```

Add a `pub(crate) fn state_for_agui(&self) -> AppState<Ctx>` to `src/server.rs` returning `self.state.clone()` (a named accessor rather than exposing the field, matching the crate's existing `agent()`/`ping_state()` style), with a `pub(crate)` doc comment.

Create `src/agui/ws.rs` for now with a stub that Task 7 fills in:

```rust
//! AG-UI `GET /ws` — bidirectional AG-UI event exchange.
```

plus a `ws_upgrade` that returns `StatusCode::NOT_IMPLEMENTED`, so this task compiles. Task 7 replaces it.

- [ ] **Step 5: Run and confirm the tests pass**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features ag-ui agui::sse`
Expected: PASS — 4 tests.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-agentcore/src/
git commit -m "feat(runtime-agentcore): SMA-461 add ag-ui sse invocations endpoint"
```

---

### Task 7: AG-UI `GET /ws`

**Files:**
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/agui/ws.rs`

**Interfaces:**
- Consumes: `EventMapper`, `RunAgentInput`, `FrameBudget` with `SplitStrategy::Content { field: "delta" }`, `AppState`.
- Produces: `pub(crate) async fn ws_upgrade<Ctx>(...)`.

Same connection lifecycle as Task 3 — fresh `RunContext` and `CancellationToken` per run, interrupt awaits the previous run's task, 2 MiB inbound cap, binary frames closed with 1003 — but the vocabulary is AG-UI and each run gets its own `EventMapper`.

- [ ] **Step 1: Write the failing tests**

Append to `src/agui/ws.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures_util::{
        stream::{self, BoxStream, StreamExt as _},
        SinkExt as _,
    };
    use paigasus_helikon_core::{
        Agent, AgentError, AgentEvent, AgentInput, RunContext, TokenUsage,
    };
    use tokio_tungstenite::tungstenite::Message;

    use crate::AgentCoreServer;

    struct TinyAgent;

    #[async_trait]
    impl Agent<()> for TinyAgent {
        fn name(&self) -> &str {
            "tiny"
        }
        fn description(&self) -> &str {
            "emits one token then completes"
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            Ok(stream::iter(vec![
                AgentEvent::TokenDelta {
                    text: "hi".to_owned(),
                },
                AgentEvent::RunCompleted {
                    usage: TokenUsage::default(),
                },
            ])
            .boxed())
        }
    }

    async fn spawn() -> String {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(TinyAgent))
            .with_default_context()
            .build()
            .unwrap();
        let router = server.agui_router();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        format!("ws://{addr}/ws")
    }

    async fn read_until_finished<S>(sock: &mut S) -> Vec<String>
    where
        S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + Unpin,
    {
        let mut kinds = Vec::new();
        while let Some(Ok(msg)) = sock.next().await {
            if let Message::Text(t) = msg {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                let ty = v["type"].as_str().unwrap().to_owned();
                let done = ty == "RUN_FINISHED" || ty == "RUN_ERROR";
                kinds.push(ty);
                if done {
                    break;
                }
            }
        }
        kinds
    }

    #[tokio::test]
    async fn ws_streams_agui_events() {
        let url = spawn().await;
        let (mut sock, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        sock.send(Message::text(
            r#"{"threadId":"t1","runId":"r1","messages":[{"role":"user","content":"hi"}]}"#,
        ))
        .await
        .unwrap();
        let kinds = read_until_finished(&mut sock).await;
        assert!(kinds.contains(&"TEXT_MESSAGE_START".to_owned()), "{kinds:?}");
        assert!(kinds.contains(&"TEXT_MESSAGE_END".to_owned()), "{kinds:?}");
        assert_eq!(kinds.last().unwrap(), "RUN_FINISHED");
    }

    #[tokio::test]
    async fn two_sequential_requests_on_one_connection_both_complete() {
        let url = spawn().await;
        let (mut sock, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        for run in ["r1", "r2"] {
            let body = format!(
                r#"{{"threadId":"t1","runId":"{run}","messages":[{{"role":"user","content":"x"}}]}}"#
            );
            sock.send(Message::text(body)).await.unwrap();
            let kinds = read_until_finished(&mut sock).await;
            assert_eq!(kinds.last().unwrap(), "RUN_FINISHED", "run {run}: {kinds:?}");
        }
    }

    #[tokio::test]
    async fn binary_frames_are_rejected_with_close_code_1003() {
        let url = spawn().await;
        let (mut sock, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        sock.send(Message::binary(vec![1, 2, 3])).await.unwrap();
        let mut code = None;
        while let Some(Ok(msg)) = sock.next().await {
            if let Message::Close(Some(f)) = msg {
                code = Some(u16::from(f.code));
                break;
            }
        }
        assert_eq!(code, Some(1003));
    }
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features ag-ui agui::ws`
Expected: FAIL — the stub returns 501, so `connect_async` errors.

- [ ] **Step 3: Implement**

Replace the stub in `src/agui/ws.rs` with the same connection structure as `src/ws.rs` (Task 3), changed in exactly four ways:

1. Parse each inbound text frame as `RunAgentInput` instead of `InvocationRequest`.
2. Build a fresh `EventMapper::new(thread_id, run_id)` per run, where `thread_id` is the header session id, else `RunAgentInput::thread_id`, else a new UUID, and `run_id` is `RunAgentInput::run_id` else a new UUID.
3. Resolve a **fresh, unshared** session per run via `InMemorySessionProvider::new(1)`, exactly as `agui/sse.rs` does and for the same reason (§6.1).
4. Construct the budget as `FrameBudget::new_with_splitter(SplitStrategy::Content { field: "delta" })`, and send `mapper.push(&ev)` frames rather than raw `AgentEvent` JSON. After the run's channel closes, send `mapper.finish()` frames too, so a stream that ends without a terminal still closes its pairs.

Keep the 2 MiB `max_message_size`, the 1003 close on binary frames, the interrupt-then-await-previous-task ordering, and the per-run `CancellationToken`. On a body that fails to parse, send a `run_error("VALIDATION_ERROR", …)` frame and continue the loop rather than closing.

- [ ] **Step 4: Run and confirm the tests pass**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features ag-ui agui::ws`
Expected: PASS — 3 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-agentcore/src/agui/ws.rs
git commit -m "feat(runtime-agentcore): SMA-461 add ag-ui websocket endpoint"
```

---

### Task 8: A2A wire types

**Files:**
- Create: `crates/paigasus-helikon-runtime-agentcore/src/a2a/types.rs`
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/a2a/mod.rs`

**Interfaces:**
- Produces, all `pub` (they appear in `TaskStore`'s signature or the builder's):
  `Task { id, context_id, status, artifacts, kind }`, `TaskStatus { state, timestamp }`,
  `TaskState` (`Submitted`/`Working`/`InputRequired`/`Completed`/`Canceled`/`Failed`, `is_terminal()`),
  `Artifact { artifact_id, name, parts }`, `Part::Text { text }`,
  `TaskEvent { seq, payload: Value }`, `AgentCard { … }`, `AgentSkill { … }`, `AgentCapabilities { streaming }`.
- Produces, `pub(crate)`: `JsonRpcRequest`, `JsonRpcResponse`, `JsonRpcError`, `rpc_error` code constants.

- [ ] **Step 1: Write the failing tests**

Create `src/a2a/types.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_serializes_in_the_documented_shape() {
        let task = Task {
            id: "task-1".to_owned(),
            context_id: "ctx-1".to_owned(),
            status: TaskStatus {
                state: TaskState::Completed,
                timestamp: "2026-08-08T09:00:00Z".to_owned(),
            },
            artifacts: vec![Artifact {
                artifact_id: "art-1".to_owned(),
                name: "agent_response".to_owned(),
                parts: vec![Part::Text {
                    text: "hello".to_owned(),
                }],
            }],
            kind: TaskKind::Task,
        };
        let v = serde_json::to_value(&task).unwrap();
        assert_eq!(v["id"], "task-1");
        assert_eq!(v["contextId"], "ctx-1");
        assert_eq!(v["status"]["state"], "completed");
        assert_eq!(v["kind"], "task");
        assert_eq!(v["artifacts"][0]["artifactId"], "art-1");
        assert_eq!(v["artifacts"][0]["parts"][0]["kind"], "text");
        assert_eq!(v["artifacts"][0]["parts"][0]["text"], "hello");
    }

    #[test]
    fn task_state_terminality_is_correct() {
        assert!(!TaskState::Submitted.is_terminal());
        assert!(!TaskState::Working.is_terminal());
        assert!(!TaskState::InputRequired.is_terminal());
        assert!(TaskState::Completed.is_terminal());
        assert!(TaskState::Canceled.is_terminal());
        assert!(TaskState::Failed.is_terminal());
    }

    #[test]
    fn parses_the_documented_message_send_request() {
        let raw = r#"{
            "jsonrpc": "2.0",
            "id": "req-001",
            "method": "message/send",
            "params": {"message": {
                "role": "user",
                "parts": [{"kind": "text", "text": "Your message content here"}],
                "messageId": "unique-message-id"
            }}
        }"#;
        let req: JsonRpcRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.method, "message/send");
        let params: MessageSendParams = serde_json::from_value(req.params.unwrap()).unwrap();
        assert_eq!(params.message.role, "user");
        assert_eq!(params.message.text(), "Your message content here");
        assert!(params.message.task_id.is_none());
    }

    #[test]
    fn message_text_concatenates_text_parts_only() {
        let raw = r#"{"role":"user","parts":[
            {"kind":"text","text":"a"},
            {"kind":"text","text":"b"}
        ],"messageId":"m"}"#;
        let m: A2aMessage = serde_json::from_str(raw).unwrap();
        assert_eq!(m.text(), "ab");
        assert!(!m.has_non_text_parts());
    }

    #[test]
    fn non_text_parts_are_detected() {
        let raw = r#"{"role":"user","parts":[
            {"kind":"file","file":{"uri":"s3://x"}}
        ],"messageId":"m"}"#;
        let m: A2aMessage = serde_json::from_str(raw).unwrap();
        assert!(m.has_non_text_parts());
    }

    #[test]
    fn error_responses_use_a2a_specification_codes() {
        let resp = JsonRpcResponse::error(
            serde_json::json!("req-001"),
            rpc_error::TASK_NOT_FOUND,
            "Task not found",
        );
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], "req-001");
        assert_eq!(v["error"]["code"], -32001);
        assert!(v.get("result").is_none(), "an error response carries no result");
    }

    /// Guard against re-introducing AWS's platform-side table (§5.6): those codes
    /// describe what the *platform* returns to a client, never what this container emits.
    #[test]
    fn specification_codes_are_not_the_aws_platform_codes() {
        assert_eq!(rpc_error::TASK_NOT_FOUND, -32001);
        assert_eq!(rpc_error::TASK_NOT_CANCELABLE, -32002);
        assert_eq!(rpc_error::PUSH_NOTIFICATION_NOT_SUPPORTED, -32003);
        assert_eq!(rpc_error::UNSUPPORTED_OPERATION, -32004);
        assert_eq!(rpc_error::CONTENT_TYPE_NOT_SUPPORTED, -32005);
        assert_eq!(rpc_error::METHOD_NOT_FOUND, -32601);
        for code in [-32051, -32052, -32053, -32054, -32055] {
            assert!(
                ![
                    rpc_error::TASK_NOT_FOUND,
                    rpc_error::TASK_NOT_CANCELABLE,
                    rpc_error::PUSH_NOTIFICATION_NOT_SUPPORTED,
                    rpc_error::UNSUPPORTED_OPERATION,
                    rpc_error::CONTENT_TYPE_NOT_SUPPORTED,
                    rpc_error::METHOD_NOT_FOUND,
                    rpc_error::INVALID_PARAMS,
                    rpc_error::INTERNAL_ERROR,
                    rpc_error::PARSE_ERROR,
                    rpc_error::INVALID_REQUEST,
                ]
                .contains(&code),
                "{code} is an AWS platform code and must not appear in this container"
            );
        }
    }

    #[test]
    fn agent_card_serializes_with_the_documented_field_names() {
        let card = AgentCard {
            name: "n".to_owned(),
            description: "d".to_owned(),
            version: "1.0.0".to_owned(),
            url: None,
            protocol_version: "0.3.0".to_owned(),
            preferred_transport: "JSONRPC".to_owned(),
            capabilities: AgentCapabilities { streaming: true },
            default_input_modes: vec!["text".to_owned()],
            default_output_modes: vec!["text".to_owned()],
            skills: vec![AgentSkill {
                id: "n".to_owned(),
                name: "n".to_owned(),
                description: "d".to_owned(),
                tags: vec![],
            }],
        };
        let v = serde_json::to_value(&card).unwrap();
        assert_eq!(v["protocolVersion"], "0.3.0");
        assert_eq!(v["preferredTransport"], "JSONRPC");
        assert_eq!(v["capabilities"]["streaming"], true);
        assert_eq!(v["defaultInputModes"][0], "text");
        assert!(
            v.get("url").is_none(),
            "an unknown url must be omitted, never published as 0.0.0.0"
        );
    }
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features a2a a2a::types`
Expected: FAIL — types not found. (Add `pub(crate) mod types;` to `src/a2a/mod.rs`.)

- [ ] **Step 3: Implement the types**

Write them above the tests in `src/a2a/types.rs`. Requirements the tests pin down:

- Every `pub` type and **every field** carries a `///` doc comment (`missing_docs` is deny-on-warn).
- `#[serde(rename_all = "camelCase")]` on `Task`, `TaskStatus`, `Artifact`, `AgentCard`, `AgentSkill`, `A2aMessage`, `MessageSendParams`.
- `TaskState` is `#[serde(rename_all = "lowercase")]` except `InputRequired`, which needs `#[serde(rename = "input-required")]`.
- `TaskKind` is a unit-ish enum serializing to `"task"`; `Part` is `#[serde(tag = "kind", rename_all = "lowercase")]` with a `Text { text: String }` variant plus `#[serde(other)] Other` so unknown part kinds deserialize rather than error (`has_non_text_parts` then reports them).
- `AgentCard::url` is `Option<String>` with `#[serde(skip_serializing_if = "Option::is_none")]`.
- `JsonRpcRequest { jsonrpc: String, id: Option<Value>, method: String, params: Option<Value> }`.
- `JsonRpcResponse` has `result(id, Value)` and `error(id, code, message)` constructors, with `result` and `error` both `#[serde(skip_serializing_if = "Option::is_none")]`.
- `A2aMessage { role, parts: Vec<Part>, message_id: Option<String>, task_id: Option<String>, context_id: Option<String> }` with `fn text(&self) -> String` and `fn has_non_text_parts(&self) -> bool`.
- `rpc_error` is a `pub(crate) mod` of `i32` constants: `PARSE_ERROR = -32700`, `INVALID_REQUEST = -32600`, `METHOD_NOT_FOUND = -32601`, `INVALID_PARAMS = -32602`, `INTERNAL_ERROR = -32603`, `TASK_NOT_FOUND = -32001`, `TASK_NOT_CANCELABLE = -32002`, `PUSH_NOTIFICATION_NOT_SUPPORTED = -32003`, `UNSUPPORTED_OPERATION = -32004`, `CONTENT_TYPE_NOT_SUPPORTED = -32005`. Give the module a doc comment stating that these are **A2A-specification** codes and that AWS's `-32051…-32055` table is platform-side and must never be emitted here.
- `TaskEvent { seq: u64, payload: Value }`, both fields documented; `seq` is assigned by the store, not the caller.
- A `pub(crate) fn now_rfc3339() -> String` using `jiff::Timestamp::now().to_string()`.

- [ ] **Step 4: Run and confirm the tests pass**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features a2a a2a::types`
Expected: PASS — 8 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-agentcore/src/a2a/
git commit -m "feat(runtime-agentcore): SMA-461 add a2a wire types"
```

---

### Task 9: `TaskStore` trait and `InMemoryTaskStore`

The `subscribe` contract is the subtle part: replay-then-live-tail with **no gap at the seam**. Getting the `Notify` ordering wrong loses an event appended between the backlog read and the wait — which is exactly what `runtime-axum`'s `EventLog` guards against, and what its `subscribe_does_not_lose_fast_appended_event` test exists for.

**Files:**
- Create: `crates/paigasus-helikon-runtime-agentcore/src/a2a/store.rs`
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/a2a/mod.rs`, `src/lib.rs` (re-exports)

**Interfaces:**
- Consumes: `crate::a2a::types::{Task, TaskState, TaskEvent}`, `crate::AgentCoreError`.
- Produces: `pub trait TaskStore` with `create`/`get`/`update_state`/`append_event`/`subscribe`; `pub struct InMemoryTaskStore` with `new(max_tasks: usize) -> Self` and `Default`.

- [ ] **Step 1: Write the failing tests**

Create `src/a2a/store.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::a2a::types::{Task, TaskKind, TaskState, TaskStatus};

    fn task(id: &str) -> Task {
        Task {
            id: id.to_owned(),
            context_id: "ctx".to_owned(),
            status: TaskStatus {
                state: TaskState::Submitted,
                timestamp: "2026-08-08T00:00:00Z".to_owned(),
            },
            artifacts: vec![],
            kind: TaskKind::Task,
        }
    }

    fn ev(n: u64) -> TaskEvent {
        TaskEvent {
            seq: 0,
            payload: serde_json::json!({"n": n}),
        }
    }

    #[tokio::test]
    async fn get_on_an_unknown_id_is_ok_none() {
        let s = InMemoryTaskStore::new(8);
        assert!(s.get("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn mutating_an_unknown_id_is_not_found() {
        let s = InMemoryTaskStore::new(8);
        let err = s
            .update_state("nope", TaskState::Submitted, TaskState::Working)
            .await
            .unwrap_err();
        assert!(matches!(err, AgentCoreError::NotFound(_)));
        let err = s.append_event("nope", ev(1)).await.unwrap_err();
        assert!(matches!(err, AgentCoreError::NotFound(_)));
        let err = s.subscribe("nope", 0).await.unwrap_err();
        assert!(matches!(err, AgentCoreError::NotFound(_)));
    }

    #[tokio::test]
    async fn append_event_returns_monotonic_sequence_numbers() {
        let s = InMemoryTaskStore::new(8);
        s.create(task("t")).await.unwrap();
        assert_eq!(s.append_event("t", ev(1)).await.unwrap(), 0);
        assert_eq!(s.append_event("t", ev(2)).await.unwrap(), 1);
        assert_eq!(s.append_event("t", ev(3)).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn update_state_is_a_compare_and_swap() {
        let s = InMemoryTaskStore::new(8);
        s.create(task("t")).await.unwrap();
        assert!(s
            .update_state("t", TaskState::Submitted, TaskState::Working)
            .await
            .unwrap());
        // The expected state no longer matches, so the swap must be refused.
        assert!(!s
            .update_state("t", TaskState::Submitted, TaskState::Canceled)
            .await
            .unwrap());
        assert_eq!(s.get("t").await.unwrap().unwrap().status.state, TaskState::Working);
    }

    /// The cancel-vs-completion race (§5.7): once the driver has written `Completed`,
    /// a late cancel must lose and leave the task completed.
    #[tokio::test]
    async fn a_late_cancel_loses_to_a_completed_task() {
        let s = InMemoryTaskStore::new(8);
        s.create(task("t")).await.unwrap();
        s.update_state("t", TaskState::Submitted, TaskState::Working)
            .await
            .unwrap();
        assert!(s
            .update_state("t", TaskState::Working, TaskState::Completed)
            .await
            .unwrap());
        assert!(!s
            .update_state("t", TaskState::Working, TaskState::Canceled)
            .await
            .unwrap());
        assert_eq!(s.get("t").await.unwrap().unwrap().status.state, TaskState::Completed);
    }

    #[tokio::test]
    async fn subscribe_replays_the_backlog_then_ends_at_the_terminal() {
        let s = InMemoryTaskStore::new(8);
        s.create(task("t")).await.unwrap();
        s.append_event("t", ev(1)).await.unwrap();
        s.append_event("t", ev(2)).await.unwrap();
        s.update_state("t", TaskState::Submitted, TaskState::Completed)
            .await
            .unwrap();

        let events: Vec<TaskEvent> = s.subscribe("t", 0).await.unwrap().collect().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 0);
        assert_eq!(events[1].seq, 1);
    }

    #[tokio::test]
    async fn subscribe_honours_the_from_cursor_inclusively() {
        let s = InMemoryTaskStore::new(8);
        s.create(task("t")).await.unwrap();
        for n in 0..4 {
            s.append_event("t", ev(n)).await.unwrap();
        }
        s.update_state("t", TaskState::Submitted, TaskState::Completed)
            .await
            .unwrap();
        let events: Vec<TaskEvent> = s.subscribe("t", 2).await.unwrap().collect().await;
        assert_eq!(events.len(), 2, "from is inclusive");
        assert_eq!(events[0].seq, 2);
    }

    /// The lost-wakeup guard, mirroring `runtime-axum`'s `EventLog` regression test: an
    /// event appended immediately after `subscribe` returns must still be delivered.
    #[tokio::test]
    async fn subscribe_does_not_lose_a_fast_appended_event() {
        let s = Arc::new(InMemoryTaskStore::new(8));
        s.create(task("t")).await.unwrap();
        let stream = s.subscribe("t", 0).await.unwrap();

        let writer = Arc::clone(&s);
        tokio::spawn(async move {
            writer.append_event("t", ev(99)).await.unwrap();
            writer
                .update_state("t", TaskState::Submitted, TaskState::Completed)
                .await
                .unwrap();
        });

        let events: Vec<TaskEvent> = stream.collect().await;
        assert_eq!(events.len(), 1, "the fast append must not be lost");
        assert_eq!(events[0].payload["n"], 99);
    }

    #[tokio::test]
    async fn live_tail_delivers_events_appended_after_subscription() {
        let s = Arc::new(InMemoryTaskStore::new(8));
        s.create(task("t")).await.unwrap();
        s.append_event("t", ev(1)).await.unwrap();
        let stream = s.subscribe("t", 0).await.unwrap();

        let writer = Arc::clone(&s);
        tokio::spawn(async move {
            for n in 2..5 {
                writer.append_event("t", ev(n)).await.unwrap();
            }
            writer
                .update_state("t", TaskState::Submitted, TaskState::Completed)
                .await
                .unwrap();
        });

        let events: Vec<TaskEvent> = stream.collect().await;
        assert_eq!(events.len(), 4, "backlog plus live events, no gap");
        let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![0, 1, 2, 3], "no duplicates and no gaps");
    }

    #[tokio::test]
    async fn the_task_count_is_bounded_by_lru_eviction() {
        let s = InMemoryTaskStore::new(2);
        s.create(task("a")).await.unwrap();
        s.create(task("b")).await.unwrap();
        s.create(task("c")).await.unwrap();
        assert!(s.get("a").await.unwrap().is_none(), "oldest task evicted");
        assert!(s.get("c").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn per_task_events_are_bounded_and_the_cursor_clamps() {
        let s = InMemoryTaskStore::new(4);
        s.create(task("t")).await.unwrap();
        for n in 0..(MAX_EVENTS_PER_TASK as u64 + 10) {
            s.append_event("t", ev(n)).await.unwrap();
        }
        s.update_state("t", TaskState::Submitted, TaskState::Completed)
            .await
            .unwrap();
        let events: Vec<TaskEvent> = s.subscribe("t", 0).await.unwrap().collect().await;
        assert_eq!(events.len(), MAX_EVENTS_PER_TASK);
        assert_eq!(events[0].seq, 10, "an evicted cursor clamps to the oldest retained event");
    }
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features a2a a2a::store`
Expected: FAIL — `cannot find trait TaskStore`.

- [ ] **Step 3: Implement**

Write above the tests, guided by these requirements:

- Module doc explaining the `subscribe` contract and the lost-wakeup ordering rule.
- `pub const MAX_EVENTS_PER_TASK: usize = 512;` — documented as bounding a single long streaming run, which task-count eviction alone does not.
- `#[async_trait] pub trait TaskStore: Send + Sync` with the five methods from §5.5, each documented with its not-found behaviour, its `Ok(false)` semantics (for `update_state`), what `append_event` returns, and that `subscribe`'s `from` is inclusive and its stream ends at the terminal state.
- `InMemoryTaskStore` holds `Mutex<Inner>` where `Inner { tasks: HashMap<String, Record>, order: VecDeque<String>, max_tasks: usize }` and `Record { task: Task, events: VecDeque<TaskEvent>, first_seq: u64, next_seq: u64, notify: Arc<Notify> }`.
- `append_event` assigns `seq = next_seq`, increments, pushes, evicts from the front and increments `first_seq` when over `MAX_EVENTS_PER_TASK`, then calls `notify.notify_waiters()`.
- `update_state` compares `record.task.status.state == expected`; on match sets the new state and `timestamp = now_rfc3339()`, calls `notify_waiters()` (so a subscriber wakes to observe terminality), and returns `Ok(true)`; otherwise `Ok(false)`.
- `subscribe` clones the `Arc<Notify>`, then returns a stream built with `futures_util::stream::unfold` over a cursor. Each poll must, **in this order**: (1) create the `Notified` future and call `.enable()` on it, (2) lock and read every event with `seq >= cursor` (clamping `cursor` up to `first_seq` and logging when it clamps) plus the current terminal flag, (3) if events are available yield them and advance the cursor, (4) else if terminal, end the stream, (5) else await the enabled `Notified` and loop. Enabling *before* the read is what closes the lost-wakeup window; a comment must say so, because the ordering looks arbitrary and is the one thing a future edit will get wrong.
- Add `notify` to the imports: `tokio::sync::Notify`.

Re-export from `src/lib.rs` behind `#[cfg(feature = "a2a")]`, each with a `///` doc line:

```rust
#[cfg(feature = "a2a")]
pub use a2a::store::{InMemoryTaskStore, TaskStore};
#[cfg(feature = "a2a")]
pub use a2a::types::{
    AgentCapabilities, AgentCard, AgentSkill, Artifact, Part, Task, TaskEvent, TaskKind,
    TaskState, TaskStatus,
};
```

- [ ] **Step 4: Run and confirm the tests pass**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features a2a a2a::store`
Expected: PASS — 11 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-agentcore/src/
git commit -m "feat(runtime-agentcore): SMA-461 add TaskStore trait and in-memory store"
```

---

### Task 10: Builder wiring and the cancel registry

**Files:**
- Create: `crates/paigasus-helikon-runtime-agentcore/src/a2a/cancel.rs`
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/server.rs`

**Interfaces:**
- Produces: `pub(crate) struct CancelRegistry` with `register(&self, task_id: String, token: CancellationToken)`, `cancel(&self, task_id: &str) -> bool`, `remove(&self, task_id: &str)`; `AppStateInner` fields `tasks`, `cancels`, `card`; builder methods `task_store`, `agent_card`, `agent_card_url`.

**Every new `AppStateInner` field and every new builder method must be `#[cfg(feature = "a2a")]`-gated**, including its initializer in `build()`. This is the most likely cause of an AC8 (`--no-default-features`) failure.

- [ ] **Step 1: Write the failing tests**

Create `src/a2a/cancel.rs`:

```rust
//! [`CancelRegistry`] — maps a live A2A task id to the `CancellationToken` driving it.
//!
//! `tasks/cancel` needs a way to reach an in-flight run from a task id. Tokens are
//! registered when a run is spawned and removed by the same detached task that owns the
//! run's lifetime, so the map cannot outlive its runs.
//!
//! A task present in the store but absent here has no live run in *this* container —
//! with a durable [`TaskStore`](crate::TaskStore) that means another microVM ran it, and
//! `tasks/cancel` answers `-32002` rather than pretending to have cancelled anything.

use std::{collections::HashMap, sync::Mutex};

use paigasus_helikon_core::CancellationToken;

/// Live-run cancellation tokens, keyed by A2A task id.
#[derive(Default)]
pub(crate) struct CancelRegistry {
    inner: Mutex<HashMap<String, CancellationToken>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_a_registered_task_fires_its_token() {
        let reg = CancelRegistry::default();
        let token = CancellationToken::new();
        reg.register("t1".to_owned(), token.clone());
        assert!(reg.cancel("t1"));
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancelling_an_unregistered_task_reports_false() {
        let reg = CancelRegistry::default();
        assert!(!reg.cancel("nope"), "no live run means nothing was cancelled");
    }

    #[test]
    fn removed_tasks_are_no_longer_cancellable() {
        let reg = CancelRegistry::default();
        reg.register("t1".to_owned(), CancellationToken::new());
        reg.remove("t1");
        assert!(!reg.cancel("t1"));
    }
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features a2a a2a::cancel`
Expected: FAIL — `no method named register`.

- [ ] **Step 3: Implement `CancelRegistry`**

Add the three methods, each with a `///` doc comment. `cancel` returns `true` only when a token was present, and fires it. `remove` drops the entry. All three take `&self` and lock the mutex; recover from a poisoned lock with `unwrap_or_else(|e| e.into_inner())` so one panicking run cannot wedge every later cancel.

- [ ] **Step 4: Wire the builder and state**

In `src/server.rs`:

- Add `pub(crate) mod` imports and, to `AppStateInner`:

```rust
    /// A2A task store backing `tasks/*`. Defaults to a bounded in-memory store.
    #[cfg(feature = "a2a")]
    pub(crate) tasks: std::sync::Arc<dyn crate::TaskStore>,
    /// Live-run cancellation tokens, keyed by A2A task id.
    #[cfg(feature = "a2a")]
    pub(crate) cancels: std::sync::Arc<crate::a2a::cancel::CancelRegistry>,
    /// Caller-supplied agent card, overriding the card derived from the agent.
    #[cfg(feature = "a2a")]
    pub(crate) card: Option<crate::AgentCard>,
    /// Caller-supplied agent-card URL, used when `AGENTCORE_RUNTIME_URL` is unset.
    #[cfg(feature = "a2a")]
    pub(crate) card_url: Option<String>,
```

- Mirror those as `Option<…>` fields on `AgentCoreServerBuilder`, also `#[cfg]`-gated.
- Add the three builder setters inside a `#[cfg(feature = "a2a")] impl` block, each documented:

```rust
    /// Override the A2A task store. Defaults to a bounded `InMemoryTaskStore`.
    ///
    /// Supply a durable store to survive AgentCore's abrupt container termination —
    /// the default loses every task with the microVM.
    pub fn task_store(mut self, store: std::sync::Arc<dyn crate::TaskStore>) -> Self { … }

    /// Replace the agent card derived from the configured agent.
    pub fn agent_card(mut self, card: crate::AgentCard) -> Self { … }

    /// Set the agent card's `url` explicitly, for deployments where
    /// `AGENTCORE_RUNTIME_URL` is not set.
    pub fn agent_card_url(mut self, url: impl Into<String>) -> Self { … }
```

- In `build()`, initialize the gated fields (`tasks` defaults to `Arc::new(InMemoryTaskStore::new(DEFAULT_MAX_TASKS))` with `const DEFAULT_MAX_TASKS: usize = 1024;` documented next to `DEFAULT_MAX_SESSIONS`).
- Add the `state_for_agui` accessor from Task 6 if it is not already present, plus an equivalent `pub(crate) fn state_for_a2a(&self) -> AppState<Ctx>`.

- [ ] **Step 5: Verify all three configurations**

Run:
```bash
cargo test  -p paigasus-helikon-runtime-agentcore --features a2a a2a::cancel
cargo build -p paigasus-helikon-runtime-agentcore --no-default-features
cargo build -p paigasus-helikon-runtime-agentcore --all-features
```
Expected: 3 tests pass; both builds succeed.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-agentcore/src/
git commit -m "feat(runtime-agentcore): SMA-461 wire a2a task store and cancel registry"
```

---

### Task 11: Agent card, `a2a_router()`, `serve_a2a()`

**Files:**
- Create: `crates/paigasus-helikon-runtime-agentcore/src/a2a/card.rs`
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/a2a/mod.rs`

**Interfaces:**
- Produces: `pub(crate) async fn agent_card<Ctx>(State<AppState<Ctx>>) -> Json<AgentCard>`; `AgentCoreServer::{a2a_router, serve_a2a}`.

- [ ] **Step 1: Write the failing tests**

Create `src/a2a/card.rs` with:

```rust
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use futures_util::stream::{self, BoxStream, StreamExt as _};
    use paigasus_helikon_core::{
        Agent, AgentError, AgentEvent, AgentInput, RunContext, TokenUsage,
    };
    use tower::ServiceExt as _;

    use crate::AgentCoreServer;

    struct NamedAgent;

    #[async_trait]
    impl Agent<()> for NamedAgent {
        fn name(&self) -> &str {
            "invoice-reconciler"
        }
        fn description(&self) -> &str {
            "reconciles invoices against statements"
        }
        async fn run(
            &self,
            _ctx: RunContext<()>,
            _input: AgentInput,
        ) -> Result<BoxStream<'static, AgentEvent>, AgentError> {
            Ok(stream::iter(vec![AgentEvent::RunCompleted {
                usage: TokenUsage::default(),
            }])
            .boxed())
        }
    }

    async fn fetch_card(server: &AgentCoreServer<()>) -> serde_json::Value {
        let resp = server
            .a2a_router()
            .oneshot(
                Request::builder()
                    .uri("/.well-known/agent-card.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn card_is_derived_from_the_configured_agent() {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(NamedAgent))
            .with_default_context()
            .build()
            .unwrap();
        let card = fetch_card(&server).await;
        assert_eq!(card["name"], "invoice-reconciler");
        assert_eq!(card["description"], "reconciles invoices against statements");
        assert_eq!(card["protocolVersion"], "0.3.0");
        assert_eq!(card["preferredTransport"], "JSONRPC");
        assert_eq!(card["capabilities"]["streaming"], true);
        assert_eq!(
            card["skills"][0]["id"], "invoice-reconciler",
            "an empty skills array is valid but useless for discovery"
        );
    }

    /// `0.0.0.0` is a bind address, not a routable URL; publishing it on a discovery
    /// card would be actively misleading, so an unknown url is omitted instead.
    #[tokio::test]
    async fn url_is_omitted_when_nothing_authoritative_is_known() {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(NamedAgent))
            .with_default_context()
            .build()
            .unwrap();
        let card = fetch_card(&server).await;
        assert!(card.get("url").is_none(), "card: {card}");
    }

    #[tokio::test]
    async fn explicit_card_url_is_published() {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(NamedAgent))
            .with_default_context()
            .agent_card_url("https://example.invalid/runtimes/x/invocations/")
            .build()
            .unwrap();
        let card = fetch_card(&server).await;
        assert_eq!(card["url"], "https://example.invalid/runtimes/x/invocations/");
    }

    #[tokio::test]
    async fn an_explicit_card_replaces_the_derived_one() {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(NamedAgent))
            .with_default_context()
            .agent_card(crate::AgentCard {
                name: "custom".to_owned(),
                description: "hand-written".to_owned(),
                version: "9.9.9".to_owned(),
                url: None,
                protocol_version: "0.3.0".to_owned(),
                preferred_transport: "JSONRPC".to_owned(),
                capabilities: crate::AgentCapabilities { streaming: true },
                default_input_modes: vec!["text".to_owned()],
                default_output_modes: vec!["text".to_owned()],
                skills: vec![],
            })
            .build()
            .unwrap();
        let card = fetch_card(&server).await;
        assert_eq!(card["name"], "custom");
        assert_eq!(card["version"], "9.9.9");
    }

    #[tokio::test]
    async fn ping_is_reachable_on_the_a2a_router() {
        let server = AgentCoreServer::builder()
            .agent(Arc::new(NamedAgent))
            .with_default_context()
            .build()
            .unwrap();
        let resp = server
            .a2a_router()
            .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
```

- [ ] **Step 2: Run and confirm failure**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features a2a a2a::card`
Expected: FAIL — `no method named a2a_router`.

- [ ] **Step 3: Implement the card handler**

Above the tests, add a module doc explaining the derivation table from §5.2 — including that `version` defaults to *this crate's* version because a library cannot read its host binary's, and that callers who need the real agent version use `.agent_card(…)`.

The handler returns `state.card.clone()` when set, else derives:

```rust
    let url = std::env::var("AGENTCORE_RUNTIME_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .or_else(|| state.card_url.clone());
```

with `name`/`description` from `state.agent.name()`/`.description()`, `version` from `env!("CARGO_PKG_VERSION")`, and one derived `AgentSkill`.

**Note for the implementer:** `url_is_omitted_when_nothing_authoritative_is_known` reads a process-global environment variable. Do not add a test that *sets* `AGENTCORE_RUNTIME_URL` — Rust test threads share the process environment and it would race the other card tests. The env-var path is covered by the `.agent_card_url(…)` test plus the `or_else` being one line.

- [ ] **Step 4: Add the router**

In `src/a2a/mod.rs`, declare the submodules and add:

```rust
/// Fixed bind address for A2A mode — distinct from HTTP (8080) and MCP (8000).
const A2A_ADDR: &str = "0.0.0.0:9000";

impl<Ctx: Send + Sync + 'static> AgentCoreServer<Ctx> {
    /// Build the A2A router: `POST /` (JSON-RPC 2.0), the agent card, and `GET /ping`.
    ///
    /// Pure: spawns nothing. Errors are **A2A-specification** JSON-RPC codes carried on
    /// an HTTP 200, per the specification — AWS's published `-32051`…`-32055` table
    /// describes what the *platform* returns to a client and is never emitted here.
    pub fn a2a_router(&self) -> Router {
        Router::new()
            .route("/ping", get(ping::ping))
            .route("/.well-known/agent-card.json", get(card::agent_card::<Ctx>))
            .route("/", post(rpc::dispatch::<Ctx>))
            .with_state(self.state_for_a2a())
    }

    /// Serve the configured agent over A2A: binds `0.0.0.0:9000` and serves
    /// [`AgentCoreServer::a2a_router`] until the process is terminated.
    ///
    /// # Errors
    ///
    /// Returns [`AgentCoreError::Internal`] if binding the listener or the serve loop
    /// fails.
    pub async fn serve_a2a(self) -> Result<(), AgentCoreError> { … }
}
```

`serve_a2a` mirrors `serve_mcp`'s body exactly (bind, log `"ready in {ms}ms"`, `axum::serve`). Create `src/a2a/rpc.rs` with a `dispatch` stub returning an `INTERNAL_ERROR` JSON-RPC response so this task compiles; Task 12 replaces it.

- [ ] **Step 5: Run and confirm the tests pass**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features a2a a2a::card`
Expected: PASS — 5 tests.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-agentcore/src/a2a/
git commit -m "feat(runtime-agentcore): SMA-461 add a2a agent card and router"
```

---

### Task 12: `message/send`, `tasks/get`, and the error taxonomy

**Files:**
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/a2a/rpc.rs`

**Interfaces:**
- Produces: `pub(crate) async fn dispatch<Ctx>(State<AppState<Ctx>>, Request) -> Response`.

- [ ] **Step 1: Write the failing tests**

Append a test module to `src/a2a/rpc.rs` covering, with a `NoopAgent` fixture and a `post_rpc(server, body) -> serde_json::Value` helper built on `a2a_router().oneshot(...)`:

```rust
    #[tokio::test]
    async fn message_send_returns_a_completed_task_with_artifacts() {
        let v = post_rpc(&server(), r#"{"jsonrpc":"2.0","id":"req-001","method":"message/send",
            "params":{"message":{"role":"user","parts":[{"kind":"text","text":"hi"}],
            "messageId":"m1"}}}"#).await;
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], "req-001");
        assert_eq!(v["result"]["kind"], "task");
        assert_eq!(v["result"]["status"]["state"], "completed");
        assert!(v["result"]["artifacts"][0]["parts"][0]["text"].is_string());
        assert!(v["result"]["id"].is_string());
        assert!(v["result"]["contextId"].is_string());
    }

    #[tokio::test]
    async fn tasks_get_returns_a_task_created_by_message_send() {
        let s = server();
        let sent = post_rpc(&s, /* message/send as above */).await;
        let id = sent["result"]["id"].as_str().unwrap();
        let got = post_rpc(&s, &format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"tasks/get","params":{{"id":"{id}"}}}}"#
        )).await;
        assert_eq!(got["result"]["id"], id);
    }

    #[tokio::test]
    async fn tasks_get_on_an_unknown_id_is_task_not_found() {
        let v = post_rpc(&server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"tasks/get","params":{"id":"nope"}}"#).await;
        assert_eq!(v["error"]["code"], -32001);
    }

    #[tokio::test]
    async fn an_unknown_method_is_method_not_found() {
        let v = post_rpc(&server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"does/notExist"}"#).await;
        assert_eq!(v["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn a_malformed_body_is_a_parse_error() {
        let v = post_rpc(&server(), "not json").await;
        assert_eq!(v["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn a_non_two_point_zero_envelope_is_an_invalid_request() {
        let v = post_rpc(&server(),
            r#"{"jsonrpc":"1.0","id":1,"method":"message/send"}"#).await;
        assert_eq!(v["error"]["code"], -32600);
    }

    #[tokio::test]
    async fn a_non_text_part_is_content_type_not_supported() {
        let v = post_rpc(&server(), r#"{"jsonrpc":"2.0","id":1,"method":"message/send",
            "params":{"message":{"role":"user",
            "parts":[{"kind":"file","file":{"uri":"s3://x"}}],"messageId":"m"}}}"#).await;
        assert_eq!(v["error"]["code"], -32005);
    }

    #[tokio::test]
    async fn push_notification_and_extended_card_methods_answer_explicitly() {
        let v = post_rpc(&server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"tasks/pushNotificationConfig/set"}"#).await;
        assert_eq!(v["error"]["code"], -32003);
        let v = post_rpc(&server(),
            r#"{"jsonrpc":"2.0","id":1,"method":"agent/authenticatedExtendedCard"}"#).await;
        assert_eq!(v["error"]["code"], -32004);
    }

    /// A JSON-RPC error rides an HTTP 200, per the A2A specification. AWS's platform
    /// returns real status codes instead; that is platform behaviour, not ours.
    #[tokio::test]
    async fn json_rpc_errors_ride_an_http_200() {
        let resp = server().a2a_router().oneshot(
            Request::builder().method("POST").uri("/")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#)).unwrap(),
        ).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn an_inbound_task_id_continues_an_existing_task() { /* send, then send again
        with params.message.taskId = the first task's id; assert result.id matches */ }

    #[tokio::test]
    async fn an_inbound_task_id_for_a_terminal_task_is_invalid_params() { /* -32602 */ }

    #[tokio::test]
    async fn an_unknown_inbound_task_id_is_task_not_found() { /* -32001 */ }

    #[tokio::test]
    async fn the_session_header_wins_over_an_inbound_context_id() { /* assert
        result.contextId equals the header value, not the client's contextId */ }
```

Write each of the four sketched bodies out in full when implementing — the pattern is identical to the tests above them.

- [ ] **Step 2: Run and confirm they fail**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features a2a a2a::rpc`
Expected: FAIL — the stub returns `INTERNAL_ERROR` for everything.

- [ ] **Step 3: Implement dispatch**

`dispatch` reads the body (2 MiB cap), extracts the session id, parses `JsonRpcRequest` (`-32700` on parse failure, `-32600` when `jsonrpc != "2.0"`), then matches on `method`:

- `"message/send"` → `send(state, session_id, params).await`
- `"tasks/get"` → store lookup, `-32001` when absent
- `"message/stream"`, `"tasks/resubscribe"` → Task 13
- `"tasks/cancel"` → Task 14
- any method starting `"tasks/pushNotificationConfig/"` → `-32003`
- `"agent/authenticatedExtendedCard"` → `-32004`
- `_` → `-32601`

`send` resolves the context id (session header wins over `message.contextId`, else a new UUID), rejects non-text parts with `-32005`, resolves or creates the task per §5.3's inbound-`taskId` table, creates it in the store as `Submitted`, CASes to `Working`, runs `Runner::run` on a **detached task** whose token is registered in the `CancelRegistry` (and removed when it finishes), awaits the result, appends a terminal `TaskEvent`, CASes to `Completed`/`Failed`, and returns the stored `Task` as `result`.

Pin the push-notification and extended-card method strings from the A2A 0.3.0 specification at implementation time and record the source in a comment — the fallthrough is a silent `-32601`, so a wrong spelling is invisible.

- [ ] **Step 4: Run and confirm the tests pass**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features a2a a2a::rpc`
Expected: PASS — 13 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-agentcore/src/a2a/rpc.rs
git commit -m "feat(runtime-agentcore): SMA-461 add a2a message/send and tasks/get"
```

---

### Task 13: `message/stream` and `tasks/resubscribe`

**Files:**
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/a2a/rpc.rs`

**Critical semantics (§5.4):** a client disconnect must **not** cancel an A2A task. Binding a `CancellationToken` drop-guard to the SSE response — as `invoke.rs` does — would cancel the task on exactly the disconnect `tasks/resubscribe` exists to survive. The detached driver still runs; only `tasks/cancel` cancels.

- [ ] **Step 1: Write the failing tests**

Append tests asserting:

```rust
    #[tokio::test]
    async fn message_stream_emits_status_then_artifact_then_final_status() {
        // POST method:"message/stream"; read the SSE body; parse each `data:` line.
        // Assert the first frame is kind:"status-update" with status.state "working"
        // and final:false; that at least one kind:"artifact-update" follows; and that
        // the last frame is kind:"status-update", state "completed", final:true.
    }

    #[tokio::test]
    async fn resubscribe_replays_a_completed_task_from_the_start() {
        // message/send, then tasks/resubscribe on its id; assert the replayed frames
        // end in a final status-update and that the stream terminates.
    }

    #[tokio::test]
    async fn resubscribe_on_an_unknown_task_is_task_not_found() {
        // -32001, delivered as a JSON-RPC error response (not an SSE stream).
    }

    /// Regression for the disconnect semantics: dropping a `message/stream` response
    /// must leave the task reachable and NOT cancelled, or `tasks/resubscribe` could
    /// only ever find cancelled tasks.
    #[tokio::test]
    async fn dropping_a_stream_leaves_the_task_resubscribable() {
        // Start message/stream against a slow agent, drop the response mid-stream,
        // then tasks/get the id and assert state is "working" or "completed",
        // never "canceled".
    }
```

Write each body out in full. For the slow agent, use a fixture whose stream yields a `TokenDelta`, then `tokio::time::sleep(Duration::from_millis(50))`, then `RunCompleted`.

- [ ] **Step 2: Run and confirm they fail**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features a2a a2a::rpc`
Expected: FAIL — `message/stream` still falls through to `-32601`.

- [ ] **Step 3: Implement**

`message/stream` creates the task exactly as `send` does, spawns the detached driver, and returns an `Sse` response fed from `TaskStore::subscribe(id, 0)` mapped to `data:` frames. The driver appends one `TaskEvent` per `AgentEvent`, shaped as A2A stream events:

- `{"taskId","contextId","kind":"status-update","status":{"state":"working","timestamp":…},"final":false}` at the start;
- `{"taskId","contextId","kind":"artifact-update","artifact":{…},"append":true,"lastChunk":false}` per text delta;
- a final `status-update` with the terminal state and `"final":true`.

`tasks/resubscribe` skips run creation and returns the same `Sse` over `subscribe(id, 0)`, answering `-32001` when the task is unknown.

**Do not** attach a `drop_guard` to either response.

- [ ] **Step 4: Run and confirm the tests pass**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features a2a a2a::rpc`
Expected: PASS — 17 tests.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-agentcore/src/a2a/rpc.rs
git commit -m "feat(runtime-agentcore): SMA-461 add a2a message/stream and resubscribe"
```

---

### Task 14: `tasks/cancel`

- [ ] **Step 1: Write the failing tests**

```rust
    #[tokio::test]
    async fn cancelling_a_live_task_transitions_it_to_canceled() {
        // message/stream against the slow agent; immediately tasks/cancel its id;
        // assert result.status.state == "canceled".
    }

    #[tokio::test]
    async fn cancelling_a_terminal_task_is_not_cancelable() {
        // message/send (completes), then tasks/cancel -> -32002.
    }

    #[tokio::test]
    async fn cancelling_an_unknown_task_is_task_not_found() {
        // -32001.
    }

    /// Regression for the CAS race (§5.7): a cancel that loses to a completed run must
    /// report -32002 AND leave the stored state `completed` — never overwrite it.
    #[tokio::test]
    async fn a_cancel_losing_the_race_leaves_the_task_completed() {
        // message/send (completes and removes its token), then tasks/cancel.
        // Assert error.code == -32002 and a follow-up tasks/get still reports
        // status.state == "completed".
    }

    /// A task in the store with no live token (a durable store, another microVM) is not
    /// cancellable from here, and must say so rather than silently succeed.
    #[tokio::test]
    async fn a_task_with_no_live_token_is_not_cancelable() {
        // Insert a Working task directly into a store handed to the builder via
        // .task_store(...), then tasks/cancel -> -32002.
    }
```

Write each body out in full.

- [ ] **Step 2: Run and confirm they fail**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features a2a a2a::rpc`
Expected: FAIL — `tasks/cancel` falls through to `-32601`.

- [ ] **Step 3: Implement**

Per §5.7's table: unknown task → `-32001`; terminal task → `-32002`; no live token → `-32002` with a message naming the reason; otherwise fire the token, then `update_state(id, Working, Canceled)` and **honour a `false` return** by answering `-32002` and leaving the stored state alone.

- [ ] **Step 4: Run and confirm the tests pass**

Run: `cargo test -p paigasus-helikon-runtime-agentcore --features a2a`
Expected: PASS — 22 tests in the `a2a` tree.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-agentcore/src/a2a/rpc.rs
git commit -m "feat(runtime-agentcore): SMA-461 add a2a tasks/cancel with cas race handling"
```

---

### Task 15: Examples

**Files:**
- Create: `crates/paigasus-helikon-runtime-agentcore/examples/{a2a_server.rs,agui_server.rs}`

Both mirror the existing `examples/echo_http.rs`: no model provider, no TLS stack, a hand-written echo `Agent`, `tracing_subscriber` init, and a `main` calling `serve_a2a()` / `serve_agui()`. Head each with a `//!` comment giving the `cargo run` line and a `curl` invocation — for A2A, the `message/send` body and the agent-card fetch from AWS's docs; for AG-UI, a `RunAgentInput` POST.

- [ ] **Step 1: Write both examples** (copy `echo_http.rs`'s agent verbatim; only `main` differs)
- [ ] **Step 2: Verify they build and run**

```bash
cargo build -p paigasus-helikon-runtime-agentcore --example a2a_server --features a2a
cargo build -p paigasus-helikon-runtime-agentcore --example agui_server --features ag-ui
```

Then run each and check by hand:

```bash
cargo run -p paigasus-helikon-runtime-agentcore --example a2a_server --features a2a &
curl -s localhost:9000/.well-known/agent-card.json | jq .
curl -s localhost:9000/ping
curl -s -X POST localhost:9000/ -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":"1","method":"message/send","params":{"message":{"role":"user","parts":[{"kind":"text","text":"hi"}],"messageId":"m1"}}}' | jq .
kill %1
```

- [ ] **Step 3: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-runtime-agentcore/examples/
git commit -m "docs(runtime-agentcore): SMA-461 add a2a and ag-ui examples"
```

---

### Task 16: Documentation

**Files:** `src/lib.rs`, `README.md`, `docker/Dockerfile`, `docs/book/src/concepts/runtimes.md`, `crates/paigasus-helikon/README.md`, root `README.md`.

- [ ] **Step 1: Crate docs (`src/lib.rs`)** — add a section per new mode after the existing MCP-mode section, each covering its port, endpoints, and contract. Must include: the §5.6 platform-vs-specification error-code trap; the §5.5 durability gap and that `TaskStore` is the seam; the §6.1 AG-UI stateless-session limitation; the §6.3 concurrent-agent limitation; and the §7.1 chunk-envelope event list. Remember: no intra-doc links from `pub` docs to `pub(crate)` items.

- [ ] **Step 2: Crate `README.md`** — contract tables for A2A, AG-UI, and `/ws` in the style of the existing HTTP/MCP tables; the four feature flags plus the `default-features = false` opt-out; the two new example commands; the CDK snippet gaining `ProtocolType.A2A` and `ProtocolType.AG_UI` next to the existing `MCP` line.

- [ ] **Step 3: `docker/Dockerfile`** — its `EXPOSE`/comment block documents "either mode's contract"; add port 9000 and mention all four modes.

- [ ] **Step 4: Book page** — in `docs/book/src/concepts/runtimes.md`, change "AgentCore recognizes two container protocols; this crate implements both" to four, extend the protocol table with A2A and AG-UI rows, and add short notes on the `FrameBudget` quota work and the concurrent-agent mapping limitation.

- [ ] **Step 5: Facade and root READMEs** — update the feature → module map wherever the agentcore row describes its protocols.

- [ ] **Step 6: Verify the docs gates**

```bash
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
mdbook build docs/book
```
Expected: all three clean. If doc coverage dips below 80%, the usual cause is a newly-`pub` wire type that should have stayed `pub(crate)` — check that first rather than adding filler docs.

- [ ] **Step 7: Commit**

```bash
git add crates/ docs/ README.md
git commit -m "docs(runtime-agentcore): SMA-461 document a2a, ag-ui and websocket modes"
```

---

### Task 17: CI gate and full local verification

**Files:** `.github/workflows/ci.yml`

- [ ] **Step 1: Extend `build-no-default-features`** — add, alongside the existing axum/actix steps:

```yaml
      - name: Build runtime-agentcore with no default features
        run: cargo build -p paigasus-helikon-runtime-agentcore --no-default-features
```

This changes a required check's content, not its name, so branch protection is unaffected.

- [ ] **Step 2: Run every CI gate locally**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
cargo build -p paigasus-helikon-runtime-agentcore --no-default-features
cargo test  -p paigasus-helikon-runtime-agentcore --no-default-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
mdbook build docs/book
convco check origin/main..HEAD
```

Expected: every command exits 0. Run `cargo test --workspace --all-features` — **not** per-crate — because feature unification differs and only the workspace-wide command matches the CI gate.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci(workflows): SMA-461 build runtime-agentcore with no default features"
```

---

## Self-Review

**Spec coverage.** §4.1 → Task 1; §4.2 → Tasks 1, 10; §5.1/§5.2 → Task 11; §5.3 → Tasks 12, 13; §5.4 → Tasks 12, 13; §5.5 → Task 9; §5.6 → Tasks 8, 12; §5.7 → Tasks 10, 14; §6.1 → Tasks 4, 6; §6.2/§6.3 → Task 5; §6.4 → Task 6; §7.1 → Task 2; §7.2 → Task 3; §7.3 → Task 7; §8 → distributed across every task; §9 → Tasks 15, 16; §10 → no task needed (release-plz handles it; nothing to do); §11 → out of scope by construction; §13 AC1–3 → Tasks 12–14, AC4 → Tasks 5–7, AC5 → Task 6, AC6 → Tasks 3, 7, AC7 → Task 2, AC8 → Tasks 1, 17, AC9 → Task 17, AC10 → Task 16.

**Type consistency.** `EventMapper::{new,push,finish}` defined in Task 5, used in Tasks 6 and 7. `FrameBudget::{new,new_with_splitter,admit}` and `SplitStrategy` defined in Task 2, used in Tasks 3 and 7. `TaskStore`'s five methods defined in Task 9, used in Tasks 12–14. `RunAgentInput::into_agent_input` defined in Task 4, used in Tasks 6 and 7. `CancelRegistry::{register,cancel,remove}` defined in Task 10, used in Tasks 12 and 14. `rpc_error` constants defined in Task 8, used in Tasks 12–14. `state_for_agui`/`state_for_a2a` introduced in Tasks 6 and 10 respectively. `InvocationRequest::into_agent_input` is made `pub(crate)` in Task 3.

**Ordering.** Task 6 stubs `agui/ws.rs` so it compiles before Task 7 fills it in; Task 11 stubs `a2a/rpc.rs::dispatch` for the same reason before Task 12. Both stubs are named in their tasks so a reviewer does not mistake them for oversights.
