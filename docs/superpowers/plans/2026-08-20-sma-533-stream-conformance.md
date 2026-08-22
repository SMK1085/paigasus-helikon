# SMA-533 Cross-Provider Stream Conformance Suite — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a cross-provider conformance suite that asserts the `Model::invoke` stream event-ordering contract against all six provider translators at the HTTP boundary, and land the `-core` contract wording it enforces.

**Architecture:** A new never-published workspace member `tests/provider-stream-conformance` holds a provider-agnostic checker plus a `hyper`-based paced HTTP server. Each of the six subjects registers by serving its own captured wire bytes through that server and handing back the stream from its real `Model::invoke`. The checker never names a provider; providers never depend on the suite.

**Tech Stack:** Rust 2021, MSRV 1.94, `tokio`, `hyper` 1.x + `hyper-util` + `http-body-util`, `aws-smithy-eventstream`, `async-trait`, `futures-util`.

**Spec:** `docs/superpowers/specs/2026-08-19-sma-533-stream-conformance-design.md`

## Global Constraints

- **Never edit any `version` field**, in any `Cargo.toml` or `CHANGELOG.md`. Spec §2.3. release-plz performs the `-core` bump itself; a manual bump defeats the dependent cascade and strands the facade.
- **Commit format:** `<type>(<scope>): SMA-533 <lowercase message>`. Scopes used here: `providers`, `core`, `docs`, `spec`, `plan`, `workflows`. `docs(plans)` is rejected by the commit-msg hook — use `docs(plan)` singular.
- **Fixture provenance:** every fixture is transcribed from captured or already-committed traffic, never invented from vendor docs. Spec §6. If a shape has no capture in the repo, do not write a fixture for it — **report it instead**.

  **One narrow exception, for binary wire formats only.** Bedrock's Converse API speaks `application/vnd.amazon.eventstream` — CRC-32-wrapped binary frames — so there is no text stream to transcribe and the rule cannot be satisfied the way it is for the four SSE providers. Its frames are instead built through the **provider SDK's own encoder** (`aws_smithy_eventstream::frame::write_message_to`), decoded by the SDK's own deserializer, with every event shape traced to the translator's own match arms rather than to vendor documentation, and **proven by execution**: the Task 3 spike drives the real `BedrockModel::invoke` against a live local endpoint and mutation-checks both failure directions.

  This exception is about *format*, not convenience. It does **not** apply to a provider with a capturable text stream — Gemini `functionCall` and OpenAI Responses tool calls both hit exactly this situation, were reported BLOCKED rather than derived, and were captured live before landing.
- **All `pub` items in `src/lib.rs` need `///` docs.** `scripts/check-doc-coverage.sh` iterates every `cargo metadata --no-deps` package excluding only `paigasus-helikon-cli`, and the crate sets `[lints] workspace = true`, so `missing_docs` is `warn` and the `docs` job runs `RUSTDOCFLAGS=-D warnings`.
- **No intra-doc link from a `pub` item to a private/`pub(crate)` item** — `rustdoc::private_intra_doc_links` fails the `docs` gate while build and tests pass. Use prose.
- **Run `cargo fmt --all` and `cargo clippy --workspace --all-features --all-targets -- -D warnings` before every commit.** The pre-commit hook is a deliberate no-op; pre-push catches it but only at push time.
- **Work synchronously.** Do not background `cargo test`/`cargo build` and end your turn — run them in the foreground and wait for terminal status.
- **Never run branch-moving git** (`checkout`, `switch`, `reset`, `stash`, `merge`, `rebase`). This is a shared checkout.
- New third-party deps go in root `[workspace.dependencies]` and are referenced as `dep.workspace = true`. Never pin a version in a member manifest.

---

### Task 1: Scaffold the crate and its core types

**Files:**
- Create: `tests/provider-stream-conformance/Cargo.toml`
- Create: `tests/provider-stream-conformance/src/lib.rs`
- Modify: `Cargo.toml` (workspace `members`, `[workspace.dependencies]`)
- Modify: `release-plz.toml` (append a `[[package]]` block)

**Interfaces:**
- Consumes: nothing.
- Produces: `Scenario` (enum, `Copy + PartialEq + Eq + Debug`), `Violation` (enum, `Debug + PartialEq`), `Outcome`, `GateHandle`, `StreamUnderTest` trait. Tasks 4–12 depend on these exact names.

- [ ] **Step 1: Register the workspace member**

`Cargo.toml:3` currently reads `members = ["crates/*", "tests/runtime-http-conformance"]`. The `tests/` entries are **enumerated, not globbed**. Change to:

```toml
members  = ["crates/*", "tests/runtime-http-conformance", "tests/provider-stream-conformance"]
```

- [ ] **Step 2: Add the new workspace dependency pins**

In root `Cargo.toml` under `[workspace.dependencies]`, keeping the existing alphabetical-ish grouping:

```toml
http-body-util        = "0.1"
hyper                 = { version = "1", default-features = false, features = ["http1", "server"] }
hyper-util            = { version = "0.1", default-features = false, features = ["tokio"] }
```

Also add, near the existing AWS block:

```toml
aws-smithy-eventstream = { version = "0.61.2", default-features = false }
```

All four are already in `Cargo.lock` transitively; these pins make them direct dependencies.

- [ ] **Step 3: Create the crate manifest**

`tests/provider-stream-conformance/Cargo.toml`:

```toml
[package]
name        = "paigasus-helikon-provider-stream-conformance"
description = "Internal: cross-provider stream event-ordering conformance suite for Paigasus Helikon."
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
async-trait    = { workspace = true }
futures-util   = { workspace = true }
http-body-util = { workspace = true }
hyper          = { workspace = true }
hyper-util     = { workspace = true }
tokio          = { workspace = true, features = ["macros", "rt-multi-thread", "net", "time", "sync"] }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }

[lints]
workspace = true
```

Provider dev-dependencies are added in Tasks 7–12, one per task, so a failure in one subject cannot block the others from compiling.

- [ ] **Step 4: Add the release-plz block**

Append to `release-plz.toml`, matching the two existing internal-crate blocks:

```toml
# Internal cross-provider stream event-ordering conformance suite. Never
# published, so it carries the same publish=false / release=false pair as the
# two internal crates above.
[[package]]
name = "paigasus-helikon-provider-stream-conformance"
publish = false
release = false
```

- [ ] **Step 5: Write the type skeleton**

`tests/provider-stream-conformance/src/lib.rs`:

```rust
//! Cross-provider conformance suite for the `Model::invoke` stream
//! event-ordering contract.
//!
//! This internal (never-published) crate hosts a provider-agnostic checker and
//! a paced HTTP server. Each subject in `tests/conformance.rs` serves its own
//! captured wire bytes through that server and hands back the stream from its
//! real `Model::invoke`, so the suite exercises the production driver and the
//! production translator together — not a reimplementation of either.
//!
//! See `docs/superpowers/specs/2026-08-19-sma-533-stream-conformance-design.md`.
#![forbid(unsafe_code)]

use futures_util::stream::BoxStream;
use paigasus_helikon_core::{CancellationToken, ModelError, ModelEvent};

/// One wire script, run against every subject that can express it.
///
/// The `a`/`b` pairs differ only in whether the script lets the translator
/// observe a stop reason before the stream ends. That distinction is the whole
/// point: with no stop reason buffered there is nothing for a broken driver to
/// wrongly flush, so the `a` variants cannot fail assertions 5 and 6 on their
/// own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// Deltas, stop reason, usage, terminator, clean EOF.
    CleanStop,
    /// Stop reason observed, then the body ends cleanly with no terminator.
    TruncatedAfterStopReason,
    /// Body ends cleanly mid-generation; no stop reason is ever observed.
    TruncatedMidGeneration,
    /// Body aborted mid-generation; no stop reason is ever observed.
    ErrorMidGeneration,
    /// Stop reason observed, then the body is aborted.
    ErrorAfterStopReason,
    /// Cancelled mid-generation; no stop reason is ever observed.
    CancelMidGeneration,
    /// Stop reason observed, then cancelled before end-of-stream.
    CancelAfterStopReason,
    /// A tool call whose name arrives split across two or more deltas.
    FragmentedToolName,
    /// One complete tool call followed by a tool-use stop reason.
    ToolCallCleanStop,
}

impl Scenario {
    /// Every scenario, in table order.
    pub const ALL: &'static [Scenario] = &[
        Scenario::CleanStop,
        Scenario::TruncatedAfterStopReason,
        Scenario::TruncatedMidGeneration,
        Scenario::ErrorMidGeneration,
        Scenario::ErrorAfterStopReason,
        Scenario::CancelMidGeneration,
        Scenario::CancelAfterStopReason,
        Scenario::FragmentedToolName,
        Scenario::ToolCallCleanStop,
    ];

    /// Whether this scenario's script must let the translator observe a stop
    /// reason. Cross-checked against each subject's own declaration so a
    /// mis-transcribed fixture cannot make assertion 3 pass vacuously.
    pub fn expects_stop_reason(self) -> bool {
        matches!(
            self,
            Scenario::CleanStop
                | Scenario::TruncatedAfterStopReason
                | Scenario::ErrorAfterStopReason
                | Scenario::CancelAfterStopReason
                | Scenario::ToolCallCleanStop
        )
    }
}

/// A contract violation, classified. Ordering matters — see `classify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// More than one `Finish` was emitted (assertion 1).
    DuplicateFinish,
    /// A `Usage` was emitted after `Finish` (assertion 2).
    UsageAfterFinish,
    /// Any other event, or an `Err`, was emitted after `Finish` (assertion 1).
    EventAfterFinish,
    /// End-of-stream after an observed stop reason emitted no `Finish`
    /// (assertion 3).
    MissingFinish,
    /// A `Finish` was emitted although no stop reason was observed
    /// (assertion 4).
    FinishOnTruncation,
    /// A `Finish` was emitted after cancellation (assertion 5).
    FinishOnCancel,
    /// A `Finish` was emitted after a mid-stream error (assertion 6).
    FinishAfterError,
    /// A `call_id` carried a number of name-bearing deltas other than one, or
    /// the name did not match the fixture's declared tool name (assertion 7).
    ToolNameNotExactlyOnce {
        /// The call whose name emission was wrong.
        call_id: String,
        /// How many deltas for that `call_id` carried `Some(name)`.
        count: usize,
    },
    /// The stream did not produce the minimum evidence its scenario requires,
    /// so the assertions would have passed vacuously.
    InsufficientEvidence(&'static str),
    /// The stream did not terminate within the per-scenario timeout.
    Timeout,
    /// The subject's `encodes_stop_reason` disagreed with the scenario's own
    /// expectation, so its fixture does not match the script it claims.
    StopReasonDeclarationMismatch {
        /// What the scenario requires.
        expected: bool,
        /// What the subject declared.
        declared: bool,
    },
}

/// Released by the harness once it has observed the gate event, letting the
/// server send the remaining chunks.
pub struct GateHandle {
    /// Signalled by the harness; the server waits on the paired receiver.
    /// Named `tx` rather than `release` so it does not shadow the method below.
    pub(crate) tx: tokio::sync::oneshot::Sender<()>,
}

impl GateHandle {
    /// Let the server send the remaining chunks.
    pub fn release(self) {
        let _ = self.tx.send(());
    }
}

/// What a subject did with a scenario.
///
/// Declining is a first-class outcome carrying a mandatory reason, not an
/// `Option` a caller can silently treat as a skip.
pub enum Outcome {
    /// The subject served the scenario.
    Served {
        /// The stream returned by the subject's `Model::invoke`.
        stream: BoxStream<'static, Result<ModelEvent, ModelError>>,
        /// Present only for the cancellation scenarios.
        gate: Option<GateHandle>,
    },
    /// The wire shape cannot physically occur for this provider. The reason is
    /// printed in the report and must match the pinned decline set.
    Declined(&'static str),
}

/// One provider backend under test.
#[async_trait::async_trait]
pub trait StreamUnderTest {
    /// Stable subject name, e.g. `"openai/chat"`. Used in failure output and to
    /// match rows in the pinned decline set.
    fn name(&self) -> &'static str;

    /// Whether this subject's fixture for `scenario` encodes a stop reason.
    /// Cross-checked against the scenario's own expectation.
    fn encodes_stop_reason(&self, scenario: Scenario) -> bool;

    /// The tool name this subject's tool-call fixtures declare.
    fn fixture_tool_name(&self) -> &'static str;

    /// Serve `scenario` and return the subject's `Model::invoke` stream.
    async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome;
}
```

- [ ] **Step 6: Verify it compiles and the gates are clean**

```bash
cargo fmt --all
cargo build -p paigasus-helikon-provider-stream-conformance
cargo clippy -p paigasus-helikon-provider-stream-conformance --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-provider-stream-conformance --no-deps
```

Expected: all four succeed. If `cargo doc` fails on a missing doc, add the `///` — do not silence the lint.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock release-plz.toml tests/provider-stream-conformance
git commit -m "feat(providers): SMA-533 scaffold the stream conformance crate"
```

---

### Task 2: The paced HTTP server

**Files:**
- Create: `tests/provider-stream-conformance/src/server.rs`
- Modify: `tests/provider-stream-conformance/src/lib.rs` — add `mod server;` plus `pub use server::{Ending, PacedServer, Script};` so later tasks reference `crate::PacedServer`, not `crate::server::PacedServer`
- Test: inline `#[cfg(test)] mod tests` in `src/server.rs`

**Interfaces:**
- Consumes: `GateHandle` from Task 1.
- Produces: `PacedServer::start(script: Script) -> PacedServer`, `PacedServer::base_url(&self) -> String`, `PacedServer::take_gate(&mut self) -> Option<GateHandle>`, and `Script { content_type: &'static str, chunks: Vec<Vec<u8>>, gate_after: Option<usize>, ending: Ending }` with `Ending::{Clean, Abort}`. Tasks 3 and 7–12 construct `Script`s.

Why hyper and not a raw `TcpListener`: a hand-rolled listener must also drain the request body before responding (or a larger request races an RST), handle `Expect: 100-continue`, and manage keep-alive — none of which serve the design, and all of which would present as provider bugs. `hyper`'s channel-backed body also gives an **abortable** body, which is what makes `Ending::Abort` reliably surface as `Err` in reqwest, `async-openai` and the smithy client.

- [ ] **Step 1: Write the failing test**

In `tests/provider-stream-conformance/src/server.rs`, at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// A clean ending must deliver every chunk and terminate the body normally.
    #[tokio::test]
    async fn clean_ending_delivers_all_chunks() {
        let server = PacedServer::start(Script {
            content_type: "text/event-stream",
            chunks: vec![b"data: one\n\n".to_vec(), b"data: two\n\n".to_vec()],
            gate_after: None,
            ending: Ending::Clean,
        })
        .await;

        let body = reqwest::Client::new()
            .post(server.base_url())
            .send()
            .await
            .expect("request should succeed")
            .text()
            .await
            .expect("clean body should read to completion");

        assert_eq!(body, "data: one\n\ndata: two\n\n");
    }

    /// An aborted ending must surface as a transport error, not a clean EOF.
    /// This is what separates scenario S4 from S3.
    #[tokio::test]
    async fn abort_ending_surfaces_as_an_error() {
        let server = PacedServer::start(Script {
            content_type: "text/event-stream",
            chunks: vec![b"data: one\n\n".to_vec()],
            gate_after: None,
            ending: Ending::Abort,
        })
        .await;

        let result = reqwest::Client::new()
            .post(server.base_url())
            .send()
            .await
            .expect("headers should arrive")
            .text()
            .await;

        assert!(
            result.is_err(),
            "an aborted body must not read as a clean EOF, got {result:?}"
        );
    }

    /// Chunks after the gate must not be sent until the gate is released.
    #[tokio::test]
    async fn gate_withholds_later_chunks_until_released() {
        let mut server = PacedServer::start(Script {
            content_type: "text/event-stream",
            chunks: vec![b"data: one\n\n".to_vec(), b"data: two\n\n".to_vec()],
            gate_after: Some(1),
            ending: Ending::Clean,
        })
        .await;
        let gate = server.take_gate().expect("gate_after was set");

        // Await the response head directly and stream its body — do NOT spawn a
        // task that buffers the whole body with `.text()`. The gate must be
        // observed frame by frame, and a spawned handle would also be unused
        // here, which `-D warnings` rejects.
        let response = reqwest::Client::new()
            .post(server.base_url())
            .send()
            .await
            .expect("headers should arrive");

        // Assert three separate properties, not one. An `is_finished()` probe
        // after a fixed sleep is one-directional: on a machine where the
        // loopback round trip exceeds the window, a completely broken gate
        // passes, and a full-body `assert_eq!` cannot rescue it because a
        // broken gate produces byte-identical output. It also never asserts
        // that the pre-gate chunk *reached the client* — the property the
        // cancellation scenarios in Tasks 9 and 11 are built on.
        let mut frames = response.bytes_stream();

        // (a) the pre-gate chunk really arrived
        let prefix = timeout(WINDOW, accumulate_until(&mut frames, "data: one\n\n")).await;
        assert!(prefix.is_ok(), "pre-gate chunk never reached the client");

        // (b) the gate withholds the next frame
        assert!(
            timeout(WINDOW, frames.next()).await.is_err(),
            "gate did not withhold the post-gate chunk"
        );

        // (c) release delivers the remainder
        gate.release();
        assert_eq!(drain(&mut frames).await, "data: two\n\n");
    }
}
```

Add `reqwest = { workspace = true, features = ["rustls"] }` to the crate's `[dev-dependencies]` for these tests.

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p paigasus-helikon-provider-stream-conformance --lib
```

Expected: FAIL to compile — `PacedServer`, `Script`, `Ending` are not defined.

- [ ] **Step 3: Implement the server**

In `src/server.rs`, above the test module. Use `hyper::server::conn::http1`, `hyper_util::rt::TokioIo`, and `http_body_util::StreamBody` fed by a `tokio::sync::mpsc` channel. Sketch of the required behaviour, to be written out fully:

- `PacedServer::start` binds `TcpListener` on `127.0.0.1:0`, records the port, and spawns an accept loop that serves **one** connection per request with `http1::Builder::new().serve_connection(TokioIo::new(stream), service_fn(...))`.
- The service handler **drains the request body first** (`body.collect().await`), then responds `200` with `content-type: <script.content_type>`.
- The response body is a `StreamBody` over an mpsc receiver. A spawned task pushes `Frame::data(chunk)` for each chunk in order; when `gate_after == Some(n)` it awaits the oneshot receiver after pushing chunk `n`.
- `Ending::Clean` drops the sender, which ends the body normally (hyper writes the terminating chunk).
- `Ending::Abort` sends an error frame — return `Err(std::io::Error::other("aborted"))` from the body stream — which makes hyper terminate the connection without the terminating chunk.
- `base_url()` returns `format!("http://127.0.0.1:{port}")`.

Do **not** use `hyper`'s `graceful_shutdown`; the abort path must be an unclean termination.

The three tests above are the full contract for this module — every behaviour listed is asserted by one of them, so implement against them rather than adding anything they do not cover.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test -p paigasus-helikon-provider-stream-conformance --lib
```

Expected: 3 passed. If `abort_ending_surfaces_as_an_error` fails because reqwest reads a clean EOF, the abort mechanism is wrong — fix it here rather than in a provider task. Every S4 assertion in this suite depends on it.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-provider-stream-conformance --all-targets -- -D warnings
git add tests/provider-stream-conformance
git commit -m "feat(providers): SMA-533 add the paced conformance http server"
```

---

### Task 3: Bedrock transport spike — retire the riskiest assumption

**Files:**
- Create: `tests/provider-stream-conformance/src/eventstream.rs`
- Modify: `tests/provider-stream-conformance/src/lib.rs` (`mod eventstream;`)
- Modify: `tests/provider-stream-conformance/Cargo.toml` (add bedrock + smithy dev-deps)
- Test: inline `#[cfg(test)] mod tests` in `src/eventstream.rs`

**Interfaces:**
- Consumes: `PacedServer`, `Script`, `Ending` from Task 2.
- Produces: `pub fn frame(event_type: &str, payload: &serde_json::Value) -> Vec<u8>`, used by Task 7.

**This task exists to fail early if it is going to fail.** Spec §11: Bedrock carries every unproven assumption and has no existing boundary coverage. Two things are unproven — that the AWS SDK will sign SigV4 and talk plain HTTP to `127.0.0.1`, and that hand-built eventstream frames decode rather than hitting the forward-compat catch-all at `bedrock/src/stream.rs:244-246`, which **silently drops** unknown variants. If either fails, stop and report; do not work around it in five other tasks.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt as _;
    use paigasus_helikon_core::{CancellationToken, ContentPart, Item, Model, ModelEvent, ModelRequest};

    /// The AWS SDK must reach a local plain-HTTP endpoint, and hand-built
    /// eventstream frames must decode into real translator output.
    ///
    /// A failure here means the whole Bedrock registration needs the
    /// `StaticReplayClient` fallback in spec §11 — report it, do not paper over
    /// it. In particular, a stream of zero `TokenDelta`s means the frames hit
    /// the forward-compat catch-all in `bedrock/src/stream.rs` and were
    /// dropped, which looks like a translator bug but is not one.
    #[tokio::test]
    async fn bedrock_reads_hand_built_frames_over_local_http() {
        let script = crate::Script {
            content_type: "application/vnd.amazon.eventstream",
            chunks: vec![
                frame("contentBlockDelta", &serde_json::json!({
                    "contentBlockIndex": 0,
                    "delta": { "text": "hi" }
                })),
                frame("messageStop", &serde_json::json!({ "stopReason": "end_turn" })),
                frame("metadata", &serde_json::json!({
                    "usage": { "inputTokens": 3, "outputTokens": 1, "totalTokens": 4 }
                })),
            ],
            gate_after: None,
            ending: crate::Ending::Clean,
        };
        let server = crate::PacedServer::start(script).await;

        let model = build_bedrock_model_against(&server.base_url());
        let mut req = ModelRequest::new();
        req.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text { text: "hi".into() }],
        }];

        let mut stream = model
            .invoke(req, CancellationToken::new())
            .await
            .expect("invoke should reach the local endpoint");

        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            events.push(ev.expect("no error event expected on a clean script"));
        }

        assert!(
            events.iter().any(|e| matches!(e, ModelEvent::TokenDelta { text } if text == "hi")),
            "expected a TokenDelta; zero means the frames were dropped as unknown, got {events:?}"
        );
        assert!(
            events.iter().any(|e| matches!(e, ModelEvent::Finish { .. })),
            "expected a terminal Finish, got {events:?}"
        );
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

```bash
cargo test -p paigasus-helikon-provider-stream-conformance --lib eventstream
```

Expected: FAIL to compile — `frame` and `build_bedrock_model_against` are not defined.

- [ ] **Step 3: Implement the frame writer and the model builder**

`frame` is a **library** item that Task 7 calls from `tests/conformance.rs`, so the
crates it uses must be normal `[dependencies]`, not `[dev-dependencies]` — a
dev-dependency is not linkable from `src/`. Only the provider crate and the SDK
entry points needed to *build a model in a test* are dev-deps.

```toml
[dependencies]
aws-smithy-eventstream = { workspace = true }
aws-smithy-types       = { workspace = true }
serde_json             = { workspace = true }

[dev-dependencies]
paigasus-helikon-providers-bedrock = { workspace = true }
aws-config             = { workspace = true }
aws-sdk-bedrockruntime = { workspace = true }
```

`frame` builds one `aws_smithy_eventstream::frame::Message` and serialises it with `write_message_to`. Three headers are mandatory and all three are string headers:

- `:message-type` = `"event"`
- `:event-type` = the union member name, **exactly** as the Bedrock model names it (`contentBlockDelta`, `messageStop`, `metadata`, `contentBlockStart`, `messageStart`)
- `:content-type` = `"application/json"`

The payload is `serde_json::to_vec(payload)`.

`build_bedrock_model_against(url)` constructs an `SdkConfig` with `endpoint_url(url)`, a static `Region::new("us-east-1")`, and hard-coded test credentials (`Credentials::new("AKIDTEST", "secret", None, None, "conformance")`), then `BedrockModel::converse("anthropic.claude-3-5-sonnet-20240620-v1:0").sdk_config(&cfg).build()`. The entry point is **`converse`**, not `builder` — verified against `crates/paigasus-helikon-providers-bedrock/src/model.rs` during the spike; there is no `BedrockModel::builder`.

- [ ] **Step 4: Run the test**

```bash
cargo test -p paigasus-helikon-provider-stream-conformance --lib eventstream -- --nocapture
```

Expected: PASS.

**If it fails, stop and report** with the observed error. Do not proceed to Task 4. The two known failure modes and their meanings:
- A transport/TLS error → the SDK will not talk plain HTTP; fall back to `StaticReplayClient` per spec §11, which also means declining S4a/S4b/S5a/S5b for Bedrock and adding the `test-util` feature to `aws-smithy-runtime`.
- Zero `TokenDelta`s, no error → the frames decoded as `Unknown` and were dropped by the catch-all. Fix the header names, not the translator.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-provider-stream-conformance --all-targets -- -D warnings
git add tests/provider-stream-conformance Cargo.toml Cargo.lock
git commit -m "feat(providers): SMA-533 prove bedrock eventstream framing over local http"
```

---

### Task 4: The checker — terminality (assertions 1 and 2)

**Files:**
- Create: `tests/provider-stream-conformance/src/check.rs`
- Create: `tests/provider-stream-conformance/src/fakes.rs`
- Modify: `tests/provider-stream-conformance/src/lib.rs` (`mod check; mod fakes;` + re-exports)

**Interfaces:**
- Consumes: `Scenario`, `Violation` from Task 1.
- Produces: `pub fn classify(events: &[Result<ModelEvent, ModelError>], scenario: Scenario, cancelled: bool) -> Option<Violation>`. Task 5 extends the same function.

**Classification is ordered**, because the rules overlap: a `Usage` after `Finish` violates both assertion 2 and assertion 1. Spec §7 fixes the order — duplicate `Finish` first, then `Usage` after `Finish`, then any other event or `Err` after `Finish`.

- [ ] **Step 1: Write the failing tests**

In `src/check.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::FinishReason;

    fn finish() -> Result<ModelEvent, ModelError> {
        Ok(ModelEvent::Finish { reason: FinishReason::Stop })
    }
    fn token(t: &str) -> Result<ModelEvent, ModelError> {
        Ok(ModelEvent::TokenDelta { text: t.into() })
    }
    fn usage() -> Result<ModelEvent, ModelError> {
        Ok(ModelEvent::Usage {
            input_tokens: 1,
            output_tokens: 1,
            cached_input_tokens: None,
            reasoning_tokens: None,
        })
    }

    /// The SMA-522 shape: usage emitted after the terminal event.
    #[test]
    fn usage_after_finish_is_classified_as_such() {
        let evs = vec![token("hi"), finish(), usage()];
        assert_eq!(
            classify(&evs, Scenario::CleanStop, false),
            Some(Violation::UsageAfterFinish)
        );
    }

    /// A second Finish outranks the "event after Finish" rule.
    #[test]
    fn double_finish_outranks_event_after_finish() {
        let evs = vec![token("hi"), finish(), finish()];
        assert_eq!(
            classify(&evs, Scenario::CleanStop, false),
            Some(Violation::DuplicateFinish)
        );
    }

    /// An Err after Finish violates terminality just as an event does.
    #[test]
    fn err_after_finish_violates_terminality() {
        let evs = vec![token("hi"), finish(), Err(ModelError::Unavailable)];
        assert_eq!(
            classify(&evs, Scenario::CleanStop, false),
            Some(Violation::EventAfterFinish)
        );
    }

    /// A conforming clean stop has no violation.
    #[test]
    fn clean_stop_conforms() {
        let evs = vec![token("hi"), usage(), finish()];
        assert_eq!(classify(&evs, Scenario::CleanStop, false), None);
    }
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cargo test -p paigasus-helikon-provider-stream-conformance --lib check
```

Expected: FAIL to compile — `classify` is not defined.

- [ ] **Step 3: Implement `classify` for assertions 1 and 2**

```rust
/// Classify the first contract violation in `events`, if any.
///
/// Rules overlap by construction — a `Usage` after `Finish` breaks both
/// assertion 2 and assertion 1 — so the order here is part of the contract, not
/// an implementation detail. Assertions 3 to 7 are added in the next task.
pub fn classify(
    events: &[Result<ModelEvent, ModelError>],
    scenario: Scenario,
    cancelled: bool,
) -> Option<Violation> {
    let finish_at = events
        .iter()
        .position(|e| matches!(e, Ok(ModelEvent::Finish { .. })));

    if let Some(idx) = finish_at {
        let after = &events[idx + 1..];
        if after
            .iter()
            .any(|e| matches!(e, Ok(ModelEvent::Finish { .. })))
        {
            return Some(Violation::DuplicateFinish);
        }
        if after.iter().any(|e| matches!(e, Ok(ModelEvent::Usage { .. }))) {
            return Some(Violation::UsageAfterFinish);
        }
        if !after.is_empty() {
            return Some(Violation::EventAfterFinish);
        }
    }

    let _ = (scenario, cancelled); // used from the next task onward
    None
}
```

- [ ] **Step 4: Run to verify they pass**

```bash
cargo test -p paigasus-helikon-provider-stream-conformance --lib check
```

Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-provider-stream-conformance --all-targets -- -D warnings
git add tests/provider-stream-conformance
git commit -m "feat(providers): SMA-533 classify finish-terminality violations"
```

---

### Task 5: The checker — emission rules and tool names (assertions 3 to 7)

**Files:**
- Modify: `tests/provider-stream-conformance/src/check.rs`

**Interfaces:**
- Consumes: `classify` from Task 4.
- Produces: the same `classify`, now complete. Task 6 calls it.

**Assertion 7 says exactly one, not at most one.** The ticket's wording is "at most one", which a translator that drops the name entirely satisfies — and that is SMA-547's bug class. `openai/backend/chat.rs:338-349` states the stakes: "a name never emitted becomes an empty name that resolves to no tool."

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `src/check.rs`:

```rust
fn tool(call_id: &str, name: Option<&str>, args: &str) -> Result<ModelEvent, ModelError> {
    Ok(ModelEvent::ToolCallDelta {
        call_id: call_id.into(),
        name: name.map(str::to_owned),
        args_delta: args.into(),
    })
}

/// The SMA-531 shape: a stop reason was observed but no Finish was emitted.
#[test]
fn missing_finish_after_observed_stop_reason() {
    let evs = vec![token("hi")];
    assert_eq!(
        classify(&evs, Scenario::TruncatedAfterStopReason, false),
        Some(Violation::MissingFinish)
    );
}

/// Truncation with no stop reason must never be reported as a clean stop.
#[test]
fn finish_on_truncation_is_a_violation() {
    let evs = vec![token("hi"), finish()];
    assert_eq!(
        classify(&evs, Scenario::TruncatedMidGeneration, false),
        Some(Violation::FinishOnTruncation)
    );
}

/// Cancellation outranks the scenario's stop-reason expectation.
#[test]
fn finish_on_cancel_is_a_violation() {
    let evs = vec![token("hi"), finish()];
    assert_eq!(
        classify(&evs, Scenario::CancelAfterStopReason, true),
        Some(Violation::FinishOnCancel)
    );
}

/// A mid-stream error must not be followed by a clean terminal event.
#[test]
fn finish_after_error_is_a_violation() {
    let evs = vec![token("hi"), Err(ModelError::Unavailable), finish()];
    assert_eq!(
        classify(&evs, Scenario::ErrorAfterStopReason, false),
        Some(Violation::FinishAfterError)
    );
}

/// The SMA-550 shape: one call_id carrying two name-bearing deltas.
#[test]
fn two_named_deltas_for_one_call_id() {
    let evs = vec![
        tool("c1", Some("get_"), "{"),
        tool("c1", Some("weather"), "}"),
        Ok(ModelEvent::Finish { reason: FinishReason::ToolCalls }),
    ];
    assert_eq!(
        classify(&evs, Scenario::ToolCallCleanStop, false),
        Some(Violation::ToolNameNotExactlyOnce { call_id: "c1".into(), count: 2 })
    );
}

/// The tightening: a call that never carries a name is also a violation.
#[test]
fn no_named_delta_for_a_call_id() {
    let evs = vec![
        tool("c1", None, "{}"),
        Ok(ModelEvent::Finish { reason: FinishReason::ToolCalls }),
    ];
    assert_eq!(
        classify(&evs, Scenario::ToolCallCleanStop, false),
        Some(Violation::ToolNameNotExactlyOnce { call_id: "c1".into(), count: 0 })
    );
}

/// A conforming tool call passes.
#[test]
fn one_named_delta_conforms() {
    let evs = vec![
        tool("c1", Some("get_weather"), "{"),
        tool("c1", None, "}"),
        Ok(ModelEvent::Finish { reason: FinishReason::ToolCalls }),
    ];
    assert_eq!(classify(&evs, Scenario::ToolCallCleanStop, false), None);
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cargo test -p paigasus-helikon-provider-stream-conformance --lib check
```

Expected: the four pre-existing tests pass; the seven new ones FAIL, each returning `None` where a `Violation` was expected.

- [ ] **Step 3: Extend `classify`**

Replace the `let _ = (scenario, cancelled);` line with, in this order:

1. **Assertion 5** — if `cancelled` and any `Finish` is present, return `FinishOnCancel`. Checked before 3/4/6 because a cancelled stream's stop-reason expectation is moot.
2. **Assertion 6** — if any `Err` is present and a `Finish` appears *after* it, return `FinishAfterError`.
3. **Assertion 4** — if `!scenario.expects_stop_reason()` and a `Finish` is present, return `FinishOnTruncation`.
4. **Assertion 3** — if `scenario.expects_stop_reason()`, **not `cancelled`**, no `Err` is present, and no `Finish` is present, return `MissingFinish`. Two guards, both load-bearing and for the same reason — another rule already governs that stream and demands the opposite outcome:
   - **no-`Err`**: assertion 6 owns the error case and requires *no* `Finish`.
   - **not-`cancelled`**: assertion 5 owns the cancelled case and also requires *no* `Finish`. Without this guard, a correctly-cancelled stream that withholds `Finish` — exactly what `core/src/model.rs` mandates — is reported as `MissingFinish`. Rule 1 does not catch it, because assertion 5 only fires when a `Finish` *is* present.
5. **Assertion 7** — group `ToolCallDelta`s by `call_id` preserving first-seen order; for each, count deltas carrying `Some(name)`; return `ToolNameNotExactlyOnce { call_id, count }` for the first group whose count is not exactly 1.

- [ ] **Step 4: Run to verify they pass**

```bash
cargo test -p paigasus-helikon-provider-stream-conformance --lib check
```

Expected: 11 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-provider-stream-conformance --all-targets -- -D warnings
git add tests/provider-stream-conformance
git commit -m "feat(providers): SMA-533 classify emission and tool-name violations"
```

---

### Task 6: The harness — floors, timeout, pinned declines, and the fakes

**Files:**
- Modify: `tests/provider-stream-conformance/src/lib.rs`
- Modify: `tests/provider-stream-conformance/src/fakes.rs`
- Create: `tests/provider-stream-conformance/src/declines.rs`

**Interfaces:**
- Consumes: `classify` (Task 5), `StreamUnderTest` / `Outcome` (Task 1).
- Produces: `pub async fn assert_conforms(subject: &impl StreamUnderTest)` and `pub const DECLINED: &[(&str, Scenario, &str)]`. Tasks 7–12 call `assert_conforms`.

Two guards live here, and both exist because of a specific way this suite could go quietly useless.

**Positive-evidence floors.** A stream that emits nothing satisfies "ends with `Finish`" trivially, and a miswired adapter serving the wrong fixture would pass every assertion by producing nothing. Floors run *before* the assertions, per spec §7.1:
- `CleanStop` through `CancelAfterStopReason` must yield ≥ 1 `TokenDelta`.
- `FragmentedToolName` and `ToolCallCleanStop` must yield ≥ 1 `ToolCallDelta`, of which exactly one carries `Some(name)` equal to `subject.fixture_tool_name()`.
- `CleanStop` and `ToolCallCleanStop` must yield exactly one `Finish`.
- `ErrorMidGeneration` and `ErrorAfterStopReason` must yield exactly one `Err`.

**The pinned decline set.** `Outcome::Declined` would otherwise be a second escape hatch with none of §9's rigor — a future engineer facing a red assertion could convert it to `Declined("wire shape cannot occur")` in one line and keep CI green. So the expected declines are pinned and the suite fails when the observed set differs **in either direction**: an unexpected decline, *and* an expected decline that stopped happening.

- [ ] **Step 1: Write the pinned decline set**

`src/declines.rs`:

```rust
use crate::Scenario;

/// Every (subject, scenario) pair that is expected to be declined, with its
/// reason. Mirrors the table in the design spec, §6.2.
///
/// The suite fails when the observed decline set differs from this in either
/// direction. Adding or removing a row is therefore a reviewed diff to a table,
/// never a one-line string literal in a match arm.
pub const DECLINED: &[(&str, Scenario, &str)] = &[
    // The tool name arrives whole in a single upstream event, so there is no
    // fragment to split.
    ("anthropic", Scenario::FragmentedToolName, "name arrives whole in content_block_start"),
    ("gemini", Scenario::FragmentedToolName, "name arrives whole in functionCall"),
    ("bedrock", Scenario::FragmentedToolName, "name arrives whole in toolUse start"),
    ("openai/responses", Scenario::FragmentedToolName, "name arrives whole in output_item.added"),
    // The window between "stop reason buffered" and "Finish emitted" lies
    // strictly between MessageStop and Metadata, with no event emitted in
    // between, so no gate edge exists.
    ("bedrock", Scenario::CancelAfterStopReason, "no observable event between MessageStop and Metadata"),
    // terminal_events builds Usage and Finish from one upstream event, so
    // "stop reason observed but no Finish yet" is not a reachable state.
    ("openai/responses", Scenario::TruncatedAfterStopReason, "stop reason and Finish are the same event"),
    ("openai/responses", Scenario::ErrorAfterStopReason, "stop reason and Finish are the same event"),
    ("openai/responses", Scenario::CancelAfterStopReason, "stop reason and Finish are the same event"),
];
```

- [ ] **Step 2: Write the failing test for the harness**

In `src/fakes.rs`, add a test that drives `assert_conforms` against a conforming fake and against each non-conforming one:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Every fake must be rejected with its own classification. A suite whose
    /// checker cannot fail is the exact failure mode that let the OpenAI bug
    /// ship past green fixtures.
    #[tokio::test]
    async fn every_fake_is_rejected_with_its_classification() {
        let cases: Vec<(Fake, Scenario, Violation)> = vec![
            (Fake::EventAfterFinish, Scenario::CleanStop, Violation::EventAfterFinish),
            (Fake::ErrAfterFinish, Scenario::CleanStop, Violation::EventAfterFinish),
            (Fake::DoubleFinish, Scenario::CleanStop, Violation::DuplicateFinish),
            (Fake::UsageAfterFinish, Scenario::CleanStop, Violation::UsageAfterFinish),
            (Fake::NoFinishAfterStopReason, Scenario::TruncatedAfterStopReason, Violation::MissingFinish),
            (Fake::FinishOnTruncation, Scenario::TruncatedMidGeneration, Violation::FinishOnTruncation),
            (Fake::FinishOnCancel, Scenario::CancelMidGeneration, Violation::FinishOnCancel),
            (Fake::FinishAfterError, Scenario::ErrorAfterStopReason, Violation::FinishAfterError),
            (Fake::TwoNamedDeltas, Scenario::ToolCallCleanStop,
             Violation::ToolNameNotExactlyOnce { call_id: "c1".into(), count: 2 }),
            (Fake::NoNamedDelta, Scenario::ToolCallCleanStop,
             Violation::ToolNameNotExactlyOnce { call_id: "c1".into(), count: 0 }),
        ];

        for (fake, scenario, expected) in cases {
            let observed = fake.run(scenario).await;
            assert_eq!(observed, Some(expected.clone()),
                "{fake:?} on {scenario:?} must be rejected as {expected:?}");
        }
    }

    /// The conforming fake must pass every scenario it serves.
    #[tokio::test]
    async fn the_conforming_fake_passes() {
        for scenario in Scenario::ALL {
            assert_eq!(Fake::Conforming.run(*scenario).await, None, "{scenario:?}");
        }
    }
}
```

- [ ] **Step 3: Run to verify it fails**

```bash
cargo test -p paigasus-helikon-provider-stream-conformance --lib fakes
```

Expected: FAIL to compile — `Fake` is not defined.

- [ ] **Step 4: Implement the fakes and `assert_conforms`**

`Fake` is a `#[derive(Debug, Clone, Copy)]` enum with the eleven variants above (ten non-conforming plus `Conforming`). These fakes test the **checker**, so they need no HTTP server: `run` builds the event vector in memory and calls `classify`.

```rust
impl Fake {
    /// Build this fake's event sequence for `scenario` and classify it.
    ///
    /// `cancelled` is derived from the scenario rather than passed in, so a
    /// fake can never be tested under a cancellation flag that contradicts the
    /// script it is emitting.
    pub async fn run(self, scenario: Scenario) -> Option<Violation> {
        let cancelled = matches!(
            scenario,
            Scenario::CancelMidGeneration | Scenario::CancelAfterStopReason
        );
        classify(&self.events(scenario), scenario, cancelled)
    }

    /// The event sequence this fake emits for `scenario`.
    fn events(self, scenario: Scenario) -> Vec<Result<ModelEvent, ModelError>> {
        // one match arm per variant; `Conforming` returns a sequence that
        // satisfies every assertion for the given scenario
        todo!("write one arm per variant")
    }
}
```

Replace the `todo!` with the real arms — it is shown only to fix the signature. `Conforming` must return, for each scenario: a `TokenDelta`, then a `Usage` and a `Finish` only when `scenario.expects_stop_reason()` and the scenario is not a cancel or error variant; a trailing `Err` for the two error scenarios; and for the two tool scenarios a `ToolCallDelta` carrying `Some("get_weather")` exactly once.

`assert_conforms` in `src/lib.rs`:

1. For each `Scenario::ALL`, call `subject.stream(scenario, token)`.
2. On `Outcome::Declined(reason)`, record `(subject.name(), scenario, reason)` and continue.
3. On `Outcome::Served { stream, gate }`: assert `subject.encodes_stop_reason(scenario) == scenario.expects_stop_reason()`, else fail with `StopReasonDeclarationMismatch`.
4. For the two cancel scenarios, spawn a task that drains the stream; wait for the stream to fall quiet, **fire the cancel token while the gate is still held**, then release the gate so the server task can exit, then collect. For all others, drain directly.

   > **Corrected during implementation.** This step originally said to release the gate *before* firing the token. That races the stream terminator against the cancellation: the server sends its remaining chunks and a clean EOF, so a **correct** provider can emit `Finish` before the token lands and be reported as `FinishOnCancel`. Holding the gate across cancellation removes the race, and `GateHandle::release`'s return value ("was the server still parked?") becomes positive evidence that truncation actually happened.
5. Wrap the whole per-scenario drain in `tokio::time::timeout(Duration::from_secs(10), …)`; on elapse, fail with `Violation::Timeout`. A subject whose stream never ends is a real bug this suite should catch, and without this it would hang `cargo test` rather than fail it.
6. Run the floors, then `classify`. Panic with the subject name, scenario and violation on any failure.
7. After the loop, compare the recorded declines against `DECLINED` filtered to this subject and panic on any difference in either direction.

- [ ] **Step 5: Run to verify it passes**

```bash
cargo test -p paigasus-helikon-provider-stream-conformance --lib
```

Expected: all pass, including the 11 checker tests from Tasks 4–5.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-provider-stream-conformance --all-targets -- -D warnings
git add tests/provider-stream-conformance
git commit -m "feat(providers): SMA-533 add conformance floors, timeout and pinned declines"
```

---

### Tasks 7 to 12: Register the six subjects

Each subject is one task, in this order: **7 bedrock, 8 openai/chat, 9 litellm, 10 anthropic, 11 gemini, 12 openai/responses.** Bedrock first because it has no existing boundary coverage at all and carries the transport risk retired in Task 3; `openai/responses` last because it declines the most scenarios and its shape is the least like the others.

**Files, per task N:**
- Create: `tests/provider-stream-conformance/tests/conformance.rs` (Task 7 only; later tasks append a module)
- Create: `tests/provider-stream-conformance/fixtures/<subject>/*.txt` (or `.bin` for bedrock)
- Modify: `tests/provider-stream-conformance/Cargo.toml` (add that provider as a dev-dep)
- Modify: `.gitattributes` (Task 7 only — see below)

**Interfaces:**
- Consumes: `assert_conforms`, `StreamUnderTest`, `Outcome`, `Scenario`, `PacedServer`, `Script`, `Ending`; for Task 7 also `eventstream::frame`.
- Produces: nothing consumed by later tasks. Subjects are independent by design, so one failing subject cannot block the others.

**The shared shape of each task:**

- [ ] **Step 1: Transcribe the fixtures**

One fixture per non-declined scenario for that subject. **Transcribe from captured or already-committed traffic** — the repo has fixture directories at `crates/paigasus-helikon-providers-litellm/tests/fixtures/`, `crates/paigasus-helikon-providers-anthropic/tests/fixtures/` and the OpenAI equivalent. Copy the provenance-header style used by `litellm/tests/fixtures/tool_call_stream_fragmented_name.txt`, which records the exact image digest it was captured from.

Two shapes are mandatory and easy to get wrong:
- **`CleanStop` must put the usage chunk *after* the stop-reason chunk.** That is the real OpenAI wire order, and SMA-522 went undetected precisely because the fixtures encoded a shape that does not occur.
- **`FragmentedToolName`** (Tasks 8 and 9 only) uses the committed capture's shape, in which both name fragments arrive **after** the id. Do not write a variant where the id resolves late — no capture in this repo supports it.

If a scenario needs a shape you cannot find a capture for, **stop and report** rather than inventing one.

- [ ] **Step 2: Write the subject and run it — expect failure first**

```rust
struct <Subject>;

#[async_trait::async_trait]
impl StreamUnderTest for <Subject> {
    fn name(&self) -> &'static str { "<subject>" }

    fn encodes_stop_reason(&self, scenario: Scenario) -> bool {
        // MEASURE the bytes about to be served. Do NOT return
        // `scenario.expects_stop_reason()` — that compares the harness's
        // expectation against itself, so the cross-check in `assert_conforms`
        // becomes dead code and a mis-transcribed fixture sails through.
        //
        // This is the only thing that can catch a lost stop reason in
        // `ErrorAfterStopReason`, whose observable events —
        // `[TokenDelta, TokenDelta, Err]` — are byte-identical to
        // `ErrorMidGeneration`'s. Assertion 3 cannot help there: its `Err`
        // guard skips it, and assertion 6 passes either way.
        script_for(scenario)
            .map(|s| s.chunks.iter().any(|c| contains(c, STOP_REASON_MARKER)))
            .unwrap_or(false)
    }

    fn fixture_tool_name(&self) -> &'static str { "get_weather" }

    async fn stream(&self, scenario: Scenario, cancel: CancellationToken) -> Outcome {
        let Some(script) = script_for(scenario) else {
            return Outcome::Declined(decline_reason(scenario));
        };
        let mut server = PacedServer::start(script).await;
        let gate = server.take_gate();
        let model = <Provider>Model::chat("test-model")
            .base_url(server.base_url())
            .build()
            .expect("builder should accept the local base url");
        let mut req = ModelRequest::new();
        req.messages = vec![Item::UserMessage {
            content: vec![ContentPart::Text { text: "hi".into() }],
        }];
        let stream = model.invoke(req, cancel).await.expect("invoke should succeed");
        Outcome::Served { stream, gate }
    }
}

#[tokio::test]
async fn <subject>_conforms() {
    assert_conforms(&<Subject>).await;
}
```

`decline_reason` returns the string from `DECLINED` for that pair, so the pinned set stays the single source of truth.

Run it:

```bash
cargo test -p paigasus-helikon-provider-stream-conformance --test conformance <subject>
```

Expected on first run: failures, naming the scenario and violation. That is the suite working. Read each one before changing anything.

- [ ] **Step 3: Triage each failure**

Per spec §9 and the approved decision:
- **A fixture or registration bug** → fix it here.
- **A small, obvious provider defect** → fix it in this task and note it in the commit body.
- **A defect needing a design decision** → **stop and surface it.** Do not add an `#[ignore]` or a new `DECLINED` row on your own initiative; both require sign-off, a filed Linear issue, and a table row.

- [ ] **Step 4: Run the subject green, then the whole suite**

```bash
cargo test -p paigasus-helikon-provider-stream-conformance
```

Expected: this subject and every previously-registered subject pass.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-provider-stream-conformance --all-targets -- -D warnings
git add tests/provider-stream-conformance Cargo.toml Cargo.lock
git commit -m "test(providers): SMA-533 register <subject> in the stream conformance suite"
```

**Task 7 additionally — `.gitattributes` for binary fixtures.** Bedrock's eventstream fixtures are **binary**. The existing rule at `.gitattributes:3-15` pins provider fixtures to `text eol=lf`; a `*.txt` glob over a binary frame file would silently corrupt it. Add, and make sure it is not shadowed by a broader `*.txt` rule:

```gitattributes
tests/provider-stream-conformance/fixtures/bedrock/** binary
tests/provider-stream-conformance/fixtures/**/*.txt text eol=lf
```

Verify with `git check-attr -a tests/provider-stream-conformance/fixtures/bedrock/<file>` that it reports `binary` and **not** `text`.

**Per-subject specifics:**

| Task | Subject | Builder entry | Declines |
| --- | --- | --- | --- |
| 7 | `bedrock` | `BedrockModel::converse(...).sdk_config(&cfg)` — **not** `builder(...)`; verified in Task 3 | `FragmentedToolName`, `CancelAfterStopReason` |
| 8 | `openai/chat` | `OpenAiModel::chat(...).base_url(...)` | none |
| 9 | `litellm` | `LiteLlmModel::chat(...).base_url(...)` | none |
| 10 | `anthropic` | `AnthropicModel::messages(...).base_url(...)` — confirm the exact constructor in that crate's `builder.rs` | `FragmentedToolName` |
| 11 | `gemini` | `GeminiModel::...(...).base_url(...)` — confirm the exact constructor | `FragmentedToolName` |
| 12 | `openai/responses` | `OpenAiModel::responses(...).base_url(...)` | `FragmentedToolName`, `TruncatedAfterStopReason`, `ErrorAfterStopReason`, `CancelAfterStopReason` |

**Facts Task 3 established, which Tasks 7–12 depend on:**

- `build_bedrock_model_against` from Task 3 is `#[cfg(test)]` in `src/`, so it is **not** reachable from `tests/conformance.rs`. Task 7 must copy its body — the exact source is in `task-3-report.md`.
- The smithy client **does not retry a mid-body abort after a 200** (one attempt only), so S4a/S4b are safe for Bedrock. It **does** retry non-2xx responses and connect failures three times — and `PacedServer` serves its script once with a gate consumed by the first request, so any scenario that provokes a retry replays against a server that will not pause.
- Stalled-stream protection is enabled with a 5 s grace. It did not fire across an 8 s gate hold in either idle or actively-polling form, but that is an observation, not a guarantee — if a Bedrock gate scenario ever fails intermittently, suspect this first.

For Task 10, note that Anthropic's `message_delta` already emits `Usage` from the same event that carries the stop reason (`anthropic/stream.rs:161-181`), so it needs no extra chunk to serve as the `CancelAfterStopReason` gate edge. For Tasks 8, 9 and 11 the stop-reason chunk emits nothing observable, so the script must place a **usage chunk** after it and gate on that.

---

### Task 13: Land the contract wording

**Files:**
- Modify: `crates/paigasus-helikon-core/src/model.rs` (three sites)
- Modify: `crates/paigasus-helikon-core/src/agent.rs:384`
- Modify: `crates/paigasus-helikon-providers-bedrock/src/stream.rs:14` and `:439`
- Modify: `docs/book/src/concepts/model-providers.md:57`

**Interfaces:** none — doc-only throughout. No behaviour changes, no signature changes.

**Four `-core` sites, not three.** `ModelEvent::ToolCallDelta` carries the positional wording **twice** — once on the variant (`model.rs:180-182`) and once on the `name` field (`model.rs:186`). Missing the variant-level one is the easy mistake.

- [ ] **Step 1: Add the emission rule to `Model::invoke`**

In the `**Event-ordering contract:**` list in `crates/paigasus-helikon-core/src/model.rs`, after the `Finish is the terminal event` bullet, add verbatim:

```
    /// - Implementations MUST emit `Finish` at end-of-stream when a stop
    ///   reason was observed, and MUST NOT emit it on truncation with no stop
    ///   reason observed, on cancellation, or after a mid-stream error.
```

- [ ] **Step 2: Replace the positional tool-name wording at all three sites**

`model.rs:180-182` (variant doc), `model.rs:186` (field doc) and `agent.rs:384` (field doc) currently describe **position** (`Some` on the first delta only). Replace each with this text, wrapped as `///` comments:

> `Some` exactly once per `call_id`, on the first delta for which the provider can establish the name is complete, and `None` on every other delta. When `Some`, the value is the whole name so far as the provider can determine — a provider receiving the name in fragments MUST buffer and concatenate them, and MUST NOT emit a name it can detect is still incomplete.

The **"can detect" qualifier is load-bearing and must survive verbatim.** A single delta carrying both `{"name":"get_","arguments":"{\"ci"}` flushes `Some("get_")` — a partial no translator can rule out without abandoning streaming names entirely. An unqualified "never emit a partial" would make the two providers SMA-547 just fixed non-conformant against a contract added in the same change.

At `agent.rs:384`, keep the existing `skip_serializing_if` paragraph and the `#[serde(...)]` attribute exactly as they are.

- [ ] **Step 3: Fix the two Bedrock comments (SMA-532)**

`bedrock/src/stream.rs:14` and `:439` both assert that `Usage` **must precede** `Finish` "per the ordering contract". The core contract says no such thing — only `Finish` is positionally constrained. Reword both to say that `Finish` is terminal and that `Usage` may appear anywhere, most providers emitting one immediately before `Finish`.

**Comment text only.** Bedrock's implementation — buffer the stop reason, pair on `Metadata`, flush at EOF — is correct and must not be touched.

- [ ] **Step 4: Update the book**

`docs/book/src/concepts/model-providers.md:57` describes `Finish { reason }` as "the terminal `Finish { reason }` (a `FinishReason`)". Extend that bullet with the emission rule from Step 1, phrased for the book's prose register. Describe it as a **contract clarification**, not a doc tweak — it tightens a public trait's contract for third-party implementors, even though it is doc-only and semver-compatible.

- [ ] **Step 5: Verify the doc gates**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
mdbook build docs/book
```

Expected: all four succeed. `mdbook build` must stay clean — `[output.linkcheck] warning-policy = "error"`.

Confirm no `version` field changed **anywhere on the branch** — not just in the
working tree. A bare `git diff` would only inspect uncommitted changes and would
pass trivially here:

```bash
git diff 0bf5e759..HEAD -- 'Cargo.toml' '*/Cargo.toml' '*CHANGELOG.md' \
  | grep -E '^[+-]version' \
  && echo "FAIL: a version field changed — see spec 2.3" \
  || echo "no version field changed — correct"
```

The new crate's own `version = "0.0.0"` line is a **new file**, not an edit to an
existing manifest, so it does not appear in this check's output as a `-version`
/ `+version` pair on a tracked manifest. If it does appear, something bumped a
real crate.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src crates/paigasus-helikon-providers-bedrock/src docs/book
git commit -m "docs(core): SMA-533 state the finish emission and tool-name completeness rules"
```

---

### Task 14: Final verification and follow-ups

**Files:**
- Modify: `docs/superpowers/plans/2026-08-20-sma-533-stream-conformance.md` (tick the boxes)

- [ ] **Step 1: Run every CI gate locally**

Exactly as CLAUDE.md specifies, job for job:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
mdbook build docs/book
```

Run `cargo test --workspace --all-features` **exactly as written** — not per-crate. Feature unification differs between the two, and a per-crate run can pass while the workspace gate goes red.

If the macOS Bedrock `NATIVE_ROOTS` failures appear (~48 failures, ~15s), note that they track the **checkout path**, not the code — a worktree under the scratchpad passes. Do not chase them as regressions caused by this branch.

- [ ] **Step 2: Confirm the decline set matches the spec**

```bash
cargo test -p paigasus-helikon-provider-stream-conformance -- --nocapture 2>&1 | grep -i declin
```

Expected: exactly the eight rows from spec §6.2, no more and no fewer.

- [ ] **Step 3: File the follow-up tickets**

Four Linear issues in the `Paigasus Helikon` project (**Linear, never GitHub issues**), each linked in the PR body:

1. **Unify the `finish()` return shape** across the six translators. Carry the four-shape table from spec §2.2 verbatim, including that `openai/backend/responses.rs` has none.
2. **`openai/chat` emits two name-carrying deltas for one `call_id`** given two deltas with different `index` but the same `id`. `backend/chat.rs:400-417` already documents it and says "closing it needs its own ticket". Note that the shape is unobserved from any backend and has no capture, which is why this suite does not cover it.
3. **`paigasus_helikon_evals::MockModel` ignores its `_cancel` argument** (`evals/src/mock.rs:48-62`), violating the cancellation clause this PR just tightened. A `Model` we ship that fails assertion 5.
4. **Extend the suite to `ReasoningDelta` ordering, parallel tool calls and zero-argument tool calls** — deliberately out of scope here.

- [ ] **Step 4: Tick every checkbox in this plan and commit**

```bash
git add docs/superpowers/plans/2026-08-20-sma-533-stream-conformance.md
git commit -m "docs(plan): SMA-533 mark the implementation plan complete"
```

---

## Task summary

| Task | Deliverable | Risk |
| --- | --- | --- |
| 1 | Crate scaffold, `Scenario`, `Violation`, `Outcome`, trait | low |
| 2 | `PacedServer` on hyper, with the abort path proven | medium |
| 3 | **Bedrock spike** — SigV4 over local HTTP + eventstream frames | **high — stop and report on failure** |
| 4 | `classify`: assertions 1 and 2, ordered | low |
| 5 | `classify`: assertions 3 to 7, "exactly one" name | low |
| 6 | Floors, timeout, pinned declines, ten fakes | medium |
| 7–12 | Six subject registrations, bedrock first | medium |
| 13 | Four `-core` doc sites, two Bedrock comments, book | low |
| 14 | Full gate run, decline audit, four follow-up tickets | low |
