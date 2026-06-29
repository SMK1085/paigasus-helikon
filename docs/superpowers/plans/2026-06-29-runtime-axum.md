# `paigasus-helikon-runtime-axum` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ascend the `paigasus-helikon-runtime-axum` stub into a self-hosted HTTP server that mounts Helikon agents and serves them over REST (one-shot), SSE, and WebSocket, with replayable runs.

**Architecture:** An `AgentServer<Ctx>` builder mounts `Arc<dyn Agent<Ctx>>`s and produces an `axum::Router`. Each run executes on a spawned task through the core `Runner<Ctx>` trait (default `TokioRunner`), draining `RunResultStreaming.events` into a per-run, bounded, append-only `EventLog` fronted by a `tokio::sync::Notify`. One-shot/SSE/WebSocket are all subscribers over the same cursor-based replay+live code path. Runs live in an in-memory `RunRegistry` with TTL + count-cap retention.

**Tech Stack:** Rust, axum 0.8, tokio, `tokio_util::sync::CancellationToken`, `futures-util`, `serde`/`serde_json`, `utoipa` (behind `openapi`), `uuid`, `thiserror`, `tracing`.

## Global Constraints

- MSRV `1.94`; edition/license/etc. inherit from `[workspace.package]` — per-crate `Cargo.toml` sets only `name`, `description`, `version`, crate-specific deps/features.
- `[lints] workspace = true` stays — every public item needs a `///` doc (`-D warnings` docs gate) and the crate is held to the **80% doc-coverage** CI gate.
- Internal deps via `{ workspace = true }`; third-party version pins live in root `[workspace.dependencies]`.
- Feature naming: kebab in `[features]`, snake in `pub use` aliases.
- Run local gates before every push: `cargo fmt --all`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`. Final pre-PR gate is the **exact** `cargo test --workspace --all-features` (not per-crate).
- Commits are signed via a 1Password SSH key (unlock the vault if a commit fails with "failed to fill whole buffer").
- Commit type/scope must satisfy `convco` (`.versionrc` allowlist). Use `feat(runtime-axum): SMA-331 …` for code; subject starts lowercase.
- Never `git add -A` (`.env`/`.claude` are untracked-but-not-ignored). Add explicit paths.
- axum 0.8 path params use `{name}`, not `:name`.
- Core API is **frozen** for this work — no edits under `crates/paigasus-helikon-core`. Everything needed is already public: `Agent<Ctx>`, `Runner<Ctx>`, `TokioRunner`, `AgentEvent`, `AgentInput`, `Item`, `TokenUsage`, `RunConfig`, `RunResultStreaming`, `RunError`, `RunContext<Ctx>`, `Session`, `MemorySession`, `AgentError`.

### Verified core signatures (do not re-derive — copy these)

```rust
// crates/paigasus-helikon-core/src/agent.rs
pub trait Agent<Ctx>: Send + Sync where Ctx: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    async fn run(&self, ctx: RunContext<Ctx>, input: AgentInput)
        -> Result<BoxStream<'static, AgentEvent>, AgentError>;        // #[async_trait]
}
pub struct AgentInput { pub messages: Vec<Item> }
impl AgentInput { pub fn new() -> Self; pub fn from_user_text(text: impl Into<String>) -> Self; }
#[serde(tag="type", rename_all="snake_case")] #[non_exhaustive]
pub enum AgentEvent { /* … */ RunCompleted { usage: TokenUsage }, RunFailed { error: String } }

// crates/paigasus-helikon-core/src/runner.rs
pub trait Runner<Ctx>: Send + Sync where Ctx: Send + Sync + 'static {
    async fn run(&self, agent: &(dyn Agent<Ctx> + '_), ctx: RunContext<Ctx>, input: AgentInput, config: RunConfig)
        -> Result<RunResult, RunError>;                              // #[async_trait]
    async fn run_streamed(&self, agent: &(dyn Agent<Ctx> + '_), ctx: RunContext<Ctx>, input: AgentInput, config: RunConfig)
        -> Result<RunResultStreaming, RunError>;
    // + resume / resume_streamed (unused here)
}
pub struct RunResultStreaming { pub events: futures_core::stream::BoxStream<'static, AgentEvent> /* … */ }
pub struct RunResult<T=String> { pub final_output: T, pub events: Vec<AgentEvent>, pub usage: TokenUsage } // NOT Serialize
#[derive(Default)] pub struct RunConfig { pub max_turns: u32, pub timeout: Option<Duration>, /* … */ }
pub struct TokenUsage { /* serde-ready */ }

// crates/paigasus-helikon-core/src/context.rs
impl<Ctx> RunContext<Ctx> {
    pub fn ephemeral(user_ctx: Ctx) -> Self;                          // in-memory session + default tracer/hooks + fresh cancel
    pub fn with_session(self, session: Arc<dyn Session>) -> Self;
    pub fn with_cancel(self, cancel: CancellationToken) -> Self;
    pub fn with_permission_mode(self, mode: PermissionMode) -> Self;  // operator security knobs
    pub fn with_approval_handler(self, h: Arc<dyn ApprovalHandler>) -> Self;
}

// crates/paigasus-helikon-core/src/session.rs
pub struct MemorySession { /* … */ } impl MemorySession { pub fn new() -> Self; }
// `TokioRunner` is a Default unit struct in paigasus-helikon-runtime-tokio.
```

> **Note on `final_output`:** the streaming path yields only `AgentEvent`s. Derive the one-shot
> `output` string by concatenating the `Text` content parts of the **last**
> `AgentEvent::MessageOutput { item }` whose `item` is an `AssistantMessage`; `usage` comes from
> the terminal `RunCompleted { usage }`; failure from `RunFailed { error }`.

---

### Task 1: Crate scaffold, dependencies & supply-chain gate

**Files:**
- Modify: `crates/paigasus-helikon-runtime-axum/Cargo.toml`
- Modify: `Cargo.toml` (root — `[workspace.dependencies]`)
- Modify: `crates/paigasus-helikon-runtime-axum/src/lib.rs`

**Interfaces:**
- Produces: a compiling crate with all deps available and `openapi` feature gating `utoipa`. No public items yet beyond crate docs.

- [ ] **Step 1: Add new third-party pins to root `Cargo.toml` `[workspace.dependencies]`**

Resolve current latest at implementation time (`cargo search uuid` / `cargo search utoipa`) and add:

```toml
uuid = { version = "<latest 1.x>", default-features = false, features = ["v4"] }
utoipa = { version = "<latest 5.x>", default-features = false, features = ["axum_extras"] }
```

- [ ] **Step 2: Write the crate `Cargo.toml`**

```toml
[package]
name = "paigasus-helikon-runtime-axum"
version = "0.0.0"            # bumped to 0.1.0 in the final ascend task, not here
edition.workspace = true
rust-version.workspace = true
description = "Self-hosted Axum HTTP/SSE/WebSocket server runtime for Paigasus Helikon agents"
publish = false             # removed in the final ascend task

[dependencies]
paigasus-helikon-core = { workspace = true }
paigasus-helikon-runtime-tokio = { workspace = true }
axum = { workspace = true, features = ["http1", "tokio", "json", "query", "ws"] }
tokio = { workspace = true }
tokio-util = { workspace = true }
futures-util = { workspace = true }
async-trait = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tracing = { workspace = true }
uuid = { workspace = true }
utoipa = { workspace = true, optional = true }

[dev-dependencies]
tokio = { workspace = true }
reqwest = { workspace = true }
tokio-tungstenite = { workspace = true }   # WS client for tests; add to workspace deps if absent

[features]
default = ["openapi"]
openapi = ["dep:utoipa"]

[lints]
workspace = true
```

> If `serde`, `reqwest`, or `tokio-tungstenite` are missing from `[workspace.dependencies]`, add them (check first with `grep -n 'serde\|reqwest\|tokio-tungstenite' Cargo.toml`). `reqwest` must use the workspace's existing TLS posture (see the rustls-CryptoProvider memory — match the existing `default-features=false` + `aws-lc-rs`/`rustls` features other crates use).

- [ ] **Step 3: Replace `src/lib.rs` stub with crate docs only**

```rust
//! Self-hosted HTTP server runtime for Paigasus Helikon agents.
//!
//! Mounts one or more [`Agent`](paigasus_helikon_core::Agent)s on an [`axum`] router and serves
//! them over REST (one-shot), Server-Sent Events, and WebSocket, with replayable runs.
//!
//! See the crate `README.md` for a runnable example.
#![forbid(unsafe_code)]

// Modules are added by subsequent tasks.
```

- [ ] **Step 4: Build**

Run: `cargo build -p paigasus-helikon-runtime-axum --all-features`
Expected: compiles clean.

- [ ] **Step 5: Supply-chain gate for the new deps (do this early)**

Run: `cargo deny check 2>&1 | tail -30` and `cargo build -p paigasus-helikon-runtime-axum` (no `openapi`, to confirm the gate works).
Expected: `deny` passes (licenses + advisories). If `utoipa`'s transitive graph trips `deny.toml` (e.g. a proc-macro advisory), pin/patch the offender via a `chore(deps)` entry — `openapi` being opt-out is the fallback. Record the outcome.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-runtime-axum/Cargo.toml Cargo.toml crates/paigasus-helikon-runtime-axum/src/lib.rs Cargo.lock
git commit -m "feat(runtime-axum): SMA-331 scaffold crate dependencies and features"
```

---

### Task 2: `error.rs` — `ServerError` + `AuthRejection`

**Files:**
- Create: `crates/paigasus-helikon-runtime-axum/src/error.rs`
- Modify: `crates/paigasus-helikon-runtime-axum/src/lib.rs` (add `mod error; pub use error::{ServerError, AuthRejection};`)
- Test: inline `#[cfg(test)]` in `error.rs`

**Interfaces:**
- Produces: `pub enum ServerError` (`#[non_exhaustive]`), `pub struct AuthRejection { pub status: StatusCode, pub message: String }`; `impl IntoResponse for ServerError`. Variants at least: `UnknownAgent(String)`, `BadRequest(String)`, `Unauthorized(AuthRejection)`, `RunStart(String)` (→500), `Unavailable(String)` (→503), `Internal(String)`.

- [ ] **Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use axum::http::StatusCode;

    #[test]
    fn status_mapping() {
        assert_eq!(ServerError::UnknownAgent("x".into()).into_response().status(), StatusCode::NOT_FOUND);
        assert_eq!(ServerError::BadRequest("x".into()).into_response().status(), StatusCode::BAD_REQUEST);
        assert_eq!(ServerError::RunStart("x".into()).into_response().status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(ServerError::Unavailable("x".into()).into_response().status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test -p paigasus-helikon-runtime-axum error::tests::status_mapping` → FAIL (module not found).

- [ ] **Step 3: Implement** — define the enums with `thiserror`, `#[non_exhaustive]`, doc every variant, and `IntoResponse` returning `(StatusCode, Json(ErrorBody))` where `ErrorBody { error: String }` is a small serde struct. Map variants per spec §6.

- [ ] **Step 4: Run to verify it passes** — same command → PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all && cargo clippy -p paigasus-helikon-runtime-axum --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-axum/src/error.rs crates/paigasus-helikon-runtime-axum/src/lib.rs
git commit -m "feat(runtime-axum): SMA-331 add ServerError and AuthRejection with status mapping"
```

---

### Task 3: `event_log.rs` — bounded append-only log + Notify subscription

**Files:**
- Create: `crates/paigasus-helikon-runtime-axum/src/event_log.rs`
- Modify: `src/lib.rs` (`mod event_log;`)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `paigasus_helikon_core::AgentEvent`.
- Produces:
  ```rust
  pub(crate) struct EventLog { /* Mutex<EventLogInner> + Notify */ }
  impl EventLog {
      pub fn new(max_events: usize) -> Self;
      pub fn append(&self, ev: AgentEvent);            // evicts head past cap; sets terminal on RunCompleted/RunFailed; notify
      pub fn mark_terminal(&self);                      // synthesize terminal w/o event (start-error path)
      pub fn read_from(&self, cursor: u64) -> ReadSlice; // { first_seq, events: Vec<AgentEvent>, next_cursor, terminal }
      pub fn subscribe(self: &Arc<Self>, from: u64) -> impl Stream<Item = AgentEvent> + Send; // replay+live, ends on terminal
  }
  pub(crate) struct ReadSlice { pub first_seq: u64, pub events: Vec<AgentEvent>, pub next_cursor: u64, pub terminal: bool }
  ```
- `is_terminal(ev: &AgentEvent) -> bool` helper (matches `RunCompleted`/`RunFailed`).

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use paigasus_helikon_core::{AgentEvent, TokenUsage};
    use futures_util::StreamExt;
    use std::sync::Arc;

    fn delta(s: &str) -> AgentEvent { AgentEvent::TokenDelta { text: s.into() } }
    fn done() -> AgentEvent { AgentEvent::RunCompleted { usage: TokenUsage::default() } }

    #[test]
    fn read_from_cursor_returns_tail_and_terminal() {
        let log = EventLog::new(1024);
        log.append(delta("a")); log.append(delta("b")); log.append(done());
        let slice = log.read_from(0);
        assert_eq!(slice.events.len(), 3);
        assert!(slice.terminal);
        assert_eq!(log.read_from(slice.next_cursor).events.len(), 0);
    }

    #[test]
    fn bounded_ring_truncates_head() {
        let log = EventLog::new(2);
        log.append(delta("a")); log.append(delta("b")); log.append(delta("c"));
        let slice = log.read_from(0);
        assert_eq!(slice.first_seq, 1);            // "a" evicted
        assert_eq!(slice.events.len(), 2);
    }

    #[tokio::test]
    async fn subscribe_replays_then_tails_until_terminal() {
        let log = Arc::new(EventLog::new(1024));
        log.append(delta("a"));
        let mut sub = log.subscribe(0);
        let l2 = log.clone();
        tokio::spawn(async move { l2.append(delta("b")); l2.append(done()); });
        let mut got = Vec::new();
        while let Some(ev) = sub.next().await { got.push(ev); }
        assert_eq!(got.len(), 3);                  // a (replay) + b + done, then stream ends
    }
}
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p paigasus-helikon-runtime-axum event_log` → FAIL.

- [ ] **Step 3: Implement.** `EventLogInner { events: VecDeque<AgentEvent>, first_seq: u64, terminal: bool }`. `append` pushes, pops front while `len > max_events` (incrementing `first_seq`), sets `terminal` via `is_terminal`, then `self.notify.notify_waiters()`. `read_from(cursor)` clamps `cursor` up to `first_seq`, returns the slice from `max(cursor, first_seq)`. `subscribe` is an `async_stream`-free manual stream: a loop that **registers `notified()` (pinned, `enable()`-d) before** calling `read_from`, yields each event, updates the cursor, returns `None` once a terminal event has been yielded. Implement with `futures_util::stream::unfold` carrying `(Arc<EventLog>, cursor, done_flag)`; on each poll: build `let fut = self.notify.notified(); tokio::pin!(fut); fut.as_mut().enable();` *before* `read_from`, drain the slice, and if not terminal, `fut.await` then loop.

- [ ] **Step 4: Run to verify they pass** → PASS (all three).

- [ ] **Step 5: fmt + clippy + commit** (`feat(runtime-axum): SMA-331 add bounded EventLog with replay+live subscription`).

---

### Task 4: `registry.rs` — run registry, handle, retention sweep

**Files:**
- Create: `crates/paigasus-helikon-runtime-axum/src/registry.rs`
- Modify: `src/lib.rs` (`mod registry;`)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `EventLog`, `tokio_util::sync::CancellationToken`, `uuid::Uuid`, `RunError`.
- Produces:
  ```rust
  pub(crate) struct RunHandle {
      pub agent_name: String,
      pub log: Arc<EventLog>,
      pub cancel: CancellationToken,
      pub start_error: Mutex<Option<String>>,
      pub terminal_at: Mutex<Option<Instant>>,
  }
  pub(crate) struct RunRegistry { /* RwLock<HashMap<Uuid, Arc<RunHandle>>> + VecDeque<(Uuid,?)> + config */ }
  impl RunRegistry {
      pub fn new(ttl: Duration, max_runs: usize, max_events_per_run: usize) -> Arc<Self>;
      pub fn create(&self, agent_name: String, cancel: CancellationToken) -> (Uuid, Arc<RunHandle>);
      pub fn get(&self, id: Uuid) -> Option<Arc<RunHandle>>;
      pub fn note_terminal(&self, id: Uuid, now: Instant);   // stamp terminal_at, push to completion queue
      pub fn sweep(&self, now: Instant);                      // TTL then FIFO-by-completion cap
      pub fn spawn_sweeper(self: &Arc<Self>);                 // OnceCell-guarded; 30s interval; Weak ref
  }
  ```

- [ ] **Step 1: Write failing tests** (drive the clock directly — no sleeps):

```rust
#[test]
fn ttl_evicts_after_deadline() {
    let reg = RunRegistry::new(Duration::from_secs(60), 1024, 1024);
    let (id, _h) = reg.create("a".into(), CancellationToken::new());
    let t0 = Instant::now();
    reg.note_terminal(id, t0);
    reg.sweep(t0 + Duration::from_secs(59));
    assert!(reg.get(id).is_some());
    reg.sweep(t0 + Duration::from_secs(61));
    assert!(reg.get(id).is_none());
}

#[test]
fn count_cap_evicts_oldest_completed_first() {
    let reg = RunRegistry::new(Duration::from_secs(3600), 2, 1024);
    let t0 = Instant::now();
    let ids: Vec<_> = (0..3).map(|i| { let (id,_) = reg.create("a".into(), CancellationToken::new()); reg.note_terminal(id, t0 + Duration::from_secs(i)); id }).collect();
    reg.sweep(t0 + Duration::from_secs(3));
    assert!(reg.get(ids[0]).is_none());            // oldest-completed evicted
    assert!(reg.get(ids[2]).is_some());
}
```

- [ ] **Step 2: Run to verify they fail** → FAIL.

- [ ] **Step 3: Implement.** `create` mints a `Uuid::new_v4()`, builds `RunHandle { log: Arc::new(EventLog::new(max_events_per_run)), … }`, inserts. `note_terminal` stamps and records completion order in a `VecDeque<Uuid>`. `sweep(now)` first drops handles where `terminal_at + ttl <= now`, then while the live-terminal count exceeds `max_runs` pops the front of the completion queue (skipping already-evicted/non-terminal ids). `spawn_sweeper` uses a `tokio::sync::OnceCell<()>` so it spawns once; the task holds `Weak<RunRegistry>` and exits when `upgrade()` returns `None`; loop `tokio::time::interval(30s)` → `reg.sweep(Instant::now())`.

- [ ] **Step 4: Run to verify they pass** → PASS.

- [ ] **Step 5: fmt + clippy + commit** (`feat(runtime-axum): SMA-331 add RunRegistry with TTL and count-cap retention`).

---

### Task 5: `session.rs` — SessionProvider + bounded in-memory provider + per-session locks

**Files:**
- Create: `crates/paigasus-helikon-runtime-axum/src/session.rs`
- Modify: `src/lib.rs` (`mod session; pub use session::{SessionProvider, InMemorySessionProvider};`)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `paigasus_helikon_core::{Session, MemorySession}`, `ServerError`.
- Produces:
  ```rust
  #[async_trait] pub trait SessionProvider: Send + Sync {
      async fn session(&self, id: Option<&str>) -> Result<Arc<dyn Session>, ServerError>;
  }
  pub struct InMemorySessionProvider { /* RwLock<HashMap<String,Arc<dyn Session>>> + max */ }
  impl InMemorySessionProvider { pub fn new(max_sessions: usize) -> Self; }
  // internal: SessionLocks { map: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> } with `lock_for(id) -> Arc<Mutex<()>>`.
  ```

- [ ] **Step 1: Write failing tests**

```rust
#[tokio::test]
async fn same_id_returns_same_session_none_is_fresh() {
    let p = InMemorySessionProvider::new(16);
    let a = p.session(Some("s1")).await.unwrap();
    let b = p.session(Some("s1")).await.unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    let anon1 = p.session(None).await.unwrap();
    let anon2 = p.session(None).await.unwrap();
    assert!(!Arc::ptr_eq(&anon1, &anon2));         // anonymous never shared / never stored
}

#[tokio::test]
async fn bounded_map_evicts() {
    let p = InMemorySessionProvider::new(1);
    let _a = p.session(Some("s1")).await.unwrap();
    let _b = p.session(Some("s2")).await.unwrap();  // evicts s1
    assert_eq!(p.len(), 1);                          // expose a test-only len()
}
```

- [ ] **Step 2: Run to verify they fail** → FAIL.

- [ ] **Step 3: Implement.** `session(Some(id))`: read-lock fast path, else write-lock, insert `Arc::new(MemorySession::new())`, evict oldest (FIFO via an insertion-order `VecDeque<String>`) when over `max_sessions`. `session(None)`: return a fresh `Arc::new(MemorySession::new())` without inserting. Add `SessionLocks` with `lock_for(id: &str) -> Arc<tokio::sync::Mutex<()>>` (create-on-miss) for Task 9's intra-session serialization; a `None` id gets a fresh throwaway lock.

- [ ] **Step 4: Run to verify they pass** → PASS.

- [ ] **Step 5: fmt + clippy + commit** (`feat(runtime-axum): SMA-331 add SessionProvider and bounded in-memory provider`).

---

### Task 6: `context.rs` — ContextProvider + default

**Files:**
- Create: `crates/paigasus-helikon-runtime-axum/src/context.rs`
- Modify: `src/lib.rs` (`mod context; pub use context::{ContextProvider, DefaultContextProvider};`)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `RunContext`, `Session`, `CancellationToken`, `axum::http::request::Parts`, `ServerError`.
- Produces:
  ```rust
  #[async_trait] pub trait ContextProvider<Ctx>: Send + Sync {
      async fn build(&self, parts: &Parts, session: Arc<dyn Session>, cancel: CancellationToken)
          -> Result<RunContext<Ctx>, ServerError>;
  }
  pub struct DefaultContextProvider; // impl<Ctx: Default> ContextProvider<Ctx>
  ```

- [ ] **Step 1: Write failing test**

```rust
#[tokio::test]
async fn default_provider_builds_context_for_unit_ctx() {
    use axum::http::Request;
    let (parts, _) = Request::builder().body(()).unwrap().into_parts();
    let session = Arc::new(paigasus_helikon_core::MemorySession::new()) as Arc<dyn paigasus_helikon_core::Session>;
    let cancel = tokio_util::sync::CancellationToken::new();
    let _ctx: RunContext<()> = DefaultContextProvider.build(&parts, session, cancel).await.unwrap();
}
```

- [ ] **Step 2: Run to verify it fails** → FAIL.

- [ ] **Step 3: Implement.** `impl<Ctx: Default + Send + Sync + 'static> ContextProvider<Ctx> for DefaultContextProvider { async fn build(&self, _parts, session, cancel) { Ok(RunContext::ephemeral(Ctx::default()).with_session(session).with_cancel(cancel)) } }`. Document that operators write their own provider to set permission mode / approval handler / hooks / tracer.

- [ ] **Step 4: Run to verify it passes** → PASS.

- [ ] **Step 5: fmt + clippy + commit** (`feat(runtime-axum): SMA-331 add ContextProvider and default for Default contexts`).

---

### Task 7: `auth.rs` — AuthLayer trait

**Files:**
- Create: `crates/paigasus-helikon-runtime-axum/src/auth.rs`
- Modify: `src/lib.rs` (`mod auth; pub use auth::AuthLayer;`)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Produces:
  ```rust
  #[async_trait] pub trait AuthLayer: Send + Sync {
      async fn authenticate(&self, parts: &mut axum::http::request::Parts) -> Result<(), AuthRejection>;
  }
  ```

- [ ] **Step 1: Write failing test** — a mock `AuthLayer` that rejects when a header is missing and, on success, inserts a `struct Identity(String)` into `parts.extensions`; assert both paths.

- [ ] **Step 2: Run to verify it fails** → FAIL.

- [ ] **Step 3: Implement** the trait + docs. (No built-in impl shipped — the test mock lives under `#[cfg(test)]`.)

- [ ] **Step 4: Run to verify it passes** → PASS.

- [ ] **Step 5: fmt + clippy + commit** (`feat(runtime-axum): SMA-331 add AuthLayer middleware trait`).

---

### Task 8: `dto.rs` — request & response DTOs

**Files:**
- Create: `crates/paigasus-helikon-runtime-axum/src/dto.rs`
- Modify: `src/lib.rs` (`mod dto;` — keep DTOs `pub` so docs/openapi can reference them)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `paigasus_helikon_core::{AgentInput, Item, AgentEvent, TokenUsage}`.
- Produces:
  ```rust
  pub struct RunRequest { /* custom Deserialize: {input:String} | {messages:Vec<Item>} */ }
  impl RunRequest { pub fn into_agent_input(self) -> AgentInput; }
  pub struct RunResponse {                         // Serialize
      pub run_id: String,
      pub status: RunStatus,                        // Completed | Failed
      pub output: Option<String>,
      pub error: Option<String>,
      pub usage: Option<TokenUsage>,
      pub events: Vec<AgentEvent>,
  }
  impl RunResponse { pub fn from_events(run_id: Uuid, events: Vec<AgentEvent>) -> Self; }
  pub struct AsyncAccepted { pub run_id: String }  // Serialize
  pub struct AgentInfo { pub name: String, pub description: String } // Serialize, for GET /agents
  ```

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn request_accepts_both_forms() {
    let a: RunRequest = serde_json::from_str(r#"{"input":"hi"}"#).unwrap();
    assert_eq!(a.into_agent_input().messages.len(), 1);
    let b: RunRequest = serde_json::from_str(r#"{"messages":[{"type":"user_message","content":[{"type":"text","text":"hi"}]}]}"#).unwrap();
    assert_eq!(b.into_agent_input().messages.len(), 1);
    assert!(serde_json::from_str::<RunRequest>(r#"{"nope":1}"#).is_err());
}

#[test]
fn response_from_events_extracts_output_and_usage() {
    let events = vec![
        AgentEvent::MessageOutput { item: assistant_text("answer") },   // helper builds an AssistantMessage Item
        AgentEvent::RunCompleted { usage: TokenUsage::default() },
    ];
    let r = RunResponse::from_events(Uuid::nil(), events);
    assert_eq!(r.status, RunStatus::Completed);
    assert_eq!(r.output.as_deref(), Some("answer"));
    assert!(r.usage.is_some());
}
```

- [ ] **Step 2: Run to verify they fail** → FAIL.

- [ ] **Step 3: Implement.** `RunRequest` via a manual `Deserialize` over an internal untagged-but-validated representation, or a `#[serde(untagged)]` helper enum wrapped so unknown shapes error. `into_agent_input`: `input` → `AgentInput::from_user_text(s)`; `messages` → `AgentInput { messages }`. `from_events`: scan for the last `MessageOutput` AssistantMessage → concat its `Text` parts into `output`; find terminal → set `status`/`usage`/`error`. When the `openapi` feature is on, derive `utoipa::ToSchema` on the response DTOs (behind `#[cfg_attr(feature="openapi", derive(utoipa::ToSchema))]`).

- [ ] **Step 4: Run to verify they pass** → PASS.

- [ ] **Step 5: fmt + clippy + commit** (`feat(runtime-axum): SMA-331 add request and response DTOs`).

---

### Task 9: `server.rs` + `handlers/agents.rs` — app state, builder, router, first route end-to-end

**Files:**
- Create: `crates/paigasus-helikon-runtime-axum/src/server.rs`
- Create: `crates/paigasus-helikon-runtime-axum/src/handlers/mod.rs`, `crates/paigasus-helikon-runtime-axum/src/handlers/agents.rs`
- Modify: `src/lib.rs` (`mod server; mod handlers; pub use server::{AgentServer, AgentServerBuilder};`)
- Test: `crates/paigasus-helikon-runtime-axum/tests/server.rs` (+ `tests/support/mod.rs` fake agent)

**Interfaces:**
- Consumes: every prior module.
- Produces: `AppState<Ctx>` (`Arc`-cloneable: registry, `Arc<dyn Runner<Ctx>>`, `HashMap<String, Arc<dyn Agent<Ctx>>>`, `Arc<dyn SessionProvider>`, `Arc<dyn ContextProvider<Ctx>>`, `Option<Arc<dyn AuthLayer>>`, `RunConfig`, `SessionLocks`); the builder and `AgentServer` from spec §5; `router(&self) -> axum::Router` wiring `GET /agents`; `serve`/`serve_with_listener`.

- [ ] **Step 1: Write the test-support fake agent** (`tests/support/mod.rs`):

```rust
use async_trait::async_trait;
use futures_util::stream::{self, BoxStream};
use paigasus_helikon_core::{Agent, AgentError, AgentEvent, AgentInput, RunContext, TokenUsage};

pub struct ScriptedAgent { pub name: String, pub events: Vec<AgentEvent> }
#[async_trait]
impl<Ctx: Send + Sync + 'static> Agent<Ctx> for ScriptedAgent {
    fn name(&self) -> &str { &self.name }
    fn description(&self) -> &str { "scripted test agent" }
    async fn run(&self, _ctx: RunContext<Ctx>, _input: AgentInput)
        -> Result<BoxStream<'static, AgentEvent>, AgentError> {
        Ok(stream::iter(self.events.clone()).boxed())
    }
}
pub fn echo_script() -> Vec<AgentEvent> { /* MessageOutput(assistant "echo") + RunCompleted */ }
```

- [ ] **Step 2: Write failing integration test** (`tests/server.rs`):

```rust
mod support;
#[tokio::test]
async fn lists_mounted_agents() {
    let server = AgentServer::<()>::builder()
        .with_default_context()
        .agent(Arc::new(support::ScriptedAgent { name: "echo".into(), events: support::echo_script() }))
        .build().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { server.serve_with_listener(listener).await.unwrap(); });
    let body: serde_json::Value = reqwest::get(format!("http://{addr}/agents")).await.unwrap().json().await.unwrap();
    assert_eq!(body[0]["name"], "echo");
}

#[test]
fn duplicate_agent_name_is_build_error() {
    let b = AgentServer::<()>::builder().with_default_context()
        .agent(Arc::new(support::ScriptedAgent{name:"x".into(),events:vec![]}))
        .agent(Arc::new(support::ScriptedAgent{name:"x".into(),events:vec![]}));
    assert!(b.build().is_err());
}

#[test]
fn build_without_context_provider_errors() {
    // Non-Default Ctx path: omitting context_provider must Err, not panic.
    let b = AgentServer::<String>::builder()
        .agent(Arc::new(support::ScriptedAgent{name:"x".into(),events:vec![]}));
    assert!(b.build().is_err());
}
```

- [ ] **Step 3: Run to verify they fail** → FAIL.

- [ ] **Step 4: Implement.** Builder accumulates agents into a `HashMap` (dup → store an error flag surfaced by `build()`); `build()` errors if no `ContextProvider` was set (defaults: runner `Arc::new(TokioRunner)`, session provider `InMemorySessionProvider::new(default_max)`); `with_default_context()` (a `where Ctx: Default` impl block) installs `DefaultContextProvider`. `router()` is pure: builds `axum::Router::new().route("/agents", get(handlers::agents::list)).with_state(state)` — **no spawn**. `agents::list` reads the state's agent map → `Vec<AgentInfo>` → `Json`. `serve_with_listener` calls `state.registry.spawn_sweeper()` then `axum::serve(listener, self.router()).await`. `serve(addr)` binds a `TcpListener` then delegates.

- [ ] **Step 5: Run to verify they pass** → PASS.

- [ ] **Step 6: fmt + clippy + commit** (`feat(runtime-axum): SMA-331 add AgentServer builder, router, and agent listing`).

---

### Task 10: `handlers/runs.rs` — one-shot, SSE, async

**Files:**
- Create: `crates/paigasus-helikon-runtime-axum/src/handlers/runs.rs`
- Modify: `src/handlers/mod.rs`, `src/server.rs` (add the three route entries on `POST /agents/{name}/runs`)
- Test: `tests/runs.rs`

**Interfaces:**
- Consumes: `AppState`, `RunRegistry`, `EventLog`, `RunRequest`/`RunResponse`, providers, `Runner`.
- Produces: a shared `spawn_run(state, agent, parts, session, input) -> (Uuid, Arc<RunHandle>)` that creates the run, builds the `RunContext` via the context provider with a fresh `CancellationToken`, and spawns the writer task; plus the three response branches keyed on the `stream`/`mode` query.

- [ ] **Step 1: Write failing tests** (`tests/runs.rs`) covering **AC1** and **AC2**:

```rust
mod support;
#[tokio::test]
async fn oneshot_returns_aggregated_result() {           // AC1
    let addr = support::spawn_echo_server().await;
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/agents/echo/runs"))
        .header("content-type", "application/json")
        .body(r#"{"input":"hello"}"#).send().await.unwrap();
    assert!(resp.headers().contains_key("x-run-id"));
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["status"], "completed");
    assert_eq!(v["output"], "echo");
}

#[tokio::test]
async fn sse_stream_matches_local_events() {             // AC2
    let addr = support::spawn_echo_server().await;
    let text = reqwest::Client::new()
        .post(format!("http://{addr}/agents/echo/runs?stream=sse"))
        .header("content-type","application/json").body(r#"{"input":"hi"}"#)
        .send().await.unwrap().text().await.unwrap();
    // parse `data:` lines → Vec<AgentEvent>; assert equals support::echo_script()
    assert_eq!(support::parse_sse(&text), support::echo_script());
}

#[tokio::test]
async fn async_mode_returns_202_then_replayable() {
    let addr = support::spawn_echo_server().await;
    let resp = reqwest::Client::new().post(format!("http://{addr}/agents/echo/runs?mode=async"))
        .header("content-type","application/json").body(r#"{"input":"hi"}"#).send().await.unwrap();
    assert_eq!(resp.status(), 202);
    let _run_id = resp.json::<serde_json::Value>().await.unwrap()["run_id"].clone();
    // (WS replay verified in Task 11)
}

#[tokio::test]
async fn unknown_agent_404() {
    let addr = support::spawn_echo_server().await;
    let resp = reqwest::Client::new().post(format!("http://{addr}/agents/nope/runs"))
        .header("content-type","application/json").body(r#"{"input":"hi"}"#).send().await.unwrap();
    assert_eq!(resp.status(), 404);
}
```

- [ ] **Step 2: Run to verify they fail** → FAIL.

- [ ] **Step 3: Implement.** Handler signature `async fn create_run(State(state), Path(name), Query(q): Query<RunQuery>, parts via FromRequestParts, Json(req): Json<RunRequest>)`. (Use a custom extractor or split: extract `Parts` first via `RequestParts`, then `Json`.) Flow: resolve agent (`404` if absent) → run auth layer if set (`401/403`) → resolve session via `state.sessions.session(x_session_id)` → acquire `state.locks.lock_for(id)` (held for the run) → `spawn_run` (creates registry entry, builds `RunContext` with a new `CancellationToken` clone, spawns writer: `match runner.run_streamed(agent.as_ref(), ctx, input, cfg).await { Ok(s) => drain s.events into log; Err(e) => { *handle.start_error.lock() = Some(e.to_string()); log.mark_terminal(); } }`, then `registry.note_terminal(id, Instant::now())` after drain). Then branch:
  - `mode=async` → return `202 Json(AsyncAccepted{run_id})` and **do not** link cancel to this connection; **release** the session lock only when the run completes (spawn a task that holds the lock guard until the writer finishes).
  - `stream=sse` → return `Sse::new(log.subscribe(0).map(to_sse_event))`; on connection drop, cancel the token (axum drops the stream → wire a `DropGuard` that cancels). Hold the session lock for the stream's lifetime.
  - default → `subscribe(0)`, collect to terminal, build `RunResponse::from_events`, return `200` with `x-run-id`; cancel token on early client drop. Release lock after completion.

  > Lock-holding detail: model the per-session lock as an owned guard moved into the run's lifetime future so concurrent same-session requests queue (Task 12 tests this).

- [ ] **Step 4: Run to verify they pass** → PASS (AC1 + AC2 now green).

- [ ] **Step 5: fmt + clippy + commit** (`feat(runtime-axum): SMA-331 add run endpoints (one-shot, SSE, async)`).

---

### Task 11: `handlers/events.rs` — WebSocket replay + tail

**Files:**
- Create: `crates/paigasus-helikon-runtime-axum/src/handlers/events.rs`
- Modify: `src/handlers/mod.rs`, `src/server.rs` (route `GET /agents/{name}/runs/{id}/events`)
- Test: `tests/ws.rs`

**Interfaces:**
- Consumes: `AppState`, `RunRegistry`, `EventLog`, `axum::extract::ws`.
- Produces: the WS upgrade handler with 404-before-upgrade semantics.

- [ ] **Step 1: Write failing tests** (use `tokio-tungstenite` against the bound port):

```rust
mod support;
#[tokio::test]
async fn ws_replays_completed_run_then_closes() {
    let addr = support::spawn_echo_server().await;
    // create a run via async mode, capture run_id
    let run_id = support::create_async_run(addr, "echo").await;
    // small yield so the run completes
    let url = format!("ws://{addr}/agents/echo/runs/{run_id}/events");
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let mut got = Vec::new();
    while let Some(Ok(msg)) = ws.next().await { if msg.is_text() { got.push(support::parse_event(msg.into_text().unwrap())); } }
    assert_eq!(got, support::echo_script());                 // full replay from seq 0
}

#[tokio::test]
async fn ws_unknown_id_404_before_upgrade() {
    let addr = support::spawn_echo_server().await;
    let url = format!("ws://{addr}/agents/echo/runs/{}/events", uuid::Uuid::nil());
    let err = tokio_tungstenite::connect_async(url).await;
    assert!(err.is_err());                                    // handshake fails (404, no 101)
}
```

- [ ] **Step 2: Run to verify they fail** → FAIL.

- [ ] **Step 3: Implement.** Handler: `Path((name, id))`, parse `id` as `Uuid` (`400`/`404` on bad/unknown), `registry.get(id)` → `404` if absent or `handle.agent_name != name`. Only then return `ws.on_upgrade(move |socket| async move { let mut sub = handle.log.subscribe(0); /* select! over sub.next() (send text frame) and socket.recv() (None ⇒ client closed ⇒ break; observers don't cancel) */ })`. Serialize each `AgentEvent` to JSON text frames. Close on terminal.

- [ ] **Step 4: Run to verify they pass** → PASS.

- [ ] **Step 5: fmt + clippy + commit** (`feat(runtime-axum): SMA-331 add WebSocket run-events endpoint with replay`).

---

### Task 12: Cancellation & intra-session serialization tests (hardening)

**Files:**
- Modify: `src/handlers/runs.rs` (only if a test exposes a gap)
- Test: `tests/concurrency.rs`

**Interfaces:** none new — verifies Task 10/11 behavior under the spec §5/§6 edge cases.

- [ ] **Step 1: Write failing/again-green tests**

```rust
mod support;
#[tokio::test]
async fn concurrent_same_session_do_not_interleave() {
    // Two slow scripted agents sharing X-Session-Id; assert the second run's session
    // snapshot already contains the first run's appended turns (serialized, not interleaved).
}
#[tokio::test]
async fn async_run_survives_creator_disconnect() {
    // POST ?mode=async, drop the client immediately, then WS-replay by id → full script present.
}
#[tokio::test]
async fn start_error_returns_500_not_hang() {
    // A SessionProvider whose `session()` returns Err → one-shot POST returns 500 within a timeout.
}
```

- [ ] **Step 2: Run** — `concurrent…` and `start_error…` may fail if the lock or start-error path is wrong; fix `runs.rs` until green. `async_run…` should already pass.

- [ ] **Step 3: fmt + clippy + commit** (`test(runtime-axum): SMA-331 cover cancellation and intra-session serialization`).

---

### Task 13: `handlers/openapi.rs` + `openapi` feature — `GET /openapi.json`

**Files:**
- Create: `crates/paigasus-helikon-runtime-axum/src/handlers/openapi.rs`
- Modify: `src/handlers/mod.rs`, `src/server.rs` (`#[cfg(feature="openapi")]` route)
- Test: `tests/openapi.rs` (gated `#![cfg(feature="openapi")]`)

**Interfaces:**
- Produces: a `utoipa::OpenApi` derive describing the generic routes + DTO schemas; the handler injects the runtime agent list into the spec's description/components before serving.

- [ ] **Step 1: Write failing test** — bind the server, `GET /openapi.json`, assert it parses as JSON, contains `"openapi"`, the `/agents/{name}/runs` path, and the mounted agent names somewhere in the document.

- [ ] **Step 2: Run to verify it fails** → FAIL.

- [ ] **Step 3: Implement.** `#[derive(utoipa::OpenApi)]` struct annotating the handlers/DTOs; handler clones the base spec, injects `Vec<AgentInfo>` (e.g. into an `x-agents` extension or the info description), returns `Json`. Gate the whole module + route + test on `feature = "openapi"`.

- [ ] **Step 4: Run to verify it passes** — `cargo test -p paigasus-helikon-runtime-axum --features openapi openapi` → PASS. Also confirm `cargo build -p paigasus-helikon-runtime-axum --no-default-features` compiles (no utoipa).

- [ ] **Step 5: fmt + clippy + commit** (`feat(runtime-axum): SMA-331 add OpenAPI document endpoint behind openapi feature`).

---

### Task 14: Example + crate README

**Files:**
- Create: `crates/paigasus-helikon-runtime-axum/examples/curl_server.rs`
- Modify: `crates/paigasus-helikon-runtime-axum/README.md`

**Interfaces:** none.

- [ ] **Step 1: Write `examples/curl_server.rs`** — mount a tiny scripted/echo agent on `AgentServer::<()>::builder().with_default_context()` and `serve("127.0.0.1:8080")`, with a top doc comment showing the `curl -H 'Content-Type: application/json' -d '{"input":"hello"}' http://localhost:8080/agents/echo/runs` invocation (AC1) and the `?stream=sse` variant (AC2).
- [ ] **Step 2: Build the example** — `cargo build -p paigasus-helikon-runtime-axum --example curl_server` → compiles.
- [ ] **Step 3: Rewrite `README.md`** from the stub to the real crate page (title + Paigasus Helikon link, `cargo add paigasus-helikon-runtime-axum`, the facade `runtime-axum` feature note, the runnable example, the route table, docs.rs/guide/GitHub links, dual-license note). Match the `paigasus-helikon-mcp/README.md` shape.
- [ ] **Step 4: Commit** (`docs(runtime-axum): SMA-331 add curl example and crate README`).

---

### Task 15: Book + facade/root docs

**Files:**
- Modify: `docs/book/src/*.md` (the runtime page — find it: `grep -rl 'runtime-axum\|Stub' docs/book/src`)
- Modify: `crates/paigasus-helikon/README.md`, `README.md` (root) — crate roster + feature→module map (stub → published)

**Interfaces:** none.

- [ ] **Step 1: Update the mdBook runtime page** to document the axum server (routes, builder, replay model) — replace any `> **Stub.**` marker.
- [ ] **Step 2: Update the facade + root README** crate roster to move `runtime-axum` from stub to published and add its feature→module row.
- [ ] **Step 3: Verify the book builds** — `mdbook build docs/book` (must be clean; linkcheck is `warning-policy = "error"`).
- [ ] **Step 4: Commit** (`docs(book): SMA-331 document the axum server runtime`).

---

### Task 16: Stub-ascend release ritual + full CI gate

**Files:**
- Modify: `crates/paigasus-helikon-runtime-axum/Cargo.toml` (version, publish), `crates/paigasus-helikon-runtime-axum/CHANGELOG.md`
- Modify: `release-plz.toml` (remove the `runtime-axum` `release = false` block)
- Modify: `Cargo.toml` (root — bump the `paigasus-helikon-runtime-axum` workspace pin to `0.1.0`)
- Modify: `crates/paigasus-helikon/Cargo.toml` (facade patch bump + its self-pin), `crates/paigasus-helikon/CHANGELOG.md`

**Interfaces:** none — release plumbing.

- [ ] **Step 1: Ascend the crate** — set `version = "0.1.0"`, remove `publish = false` from `crates/paigasus-helikon-runtime-axum/Cargo.toml`; add a `0.1.0` CHANGELOG entry.
- [ ] **Step 2: Release-plz** — delete the `[[package]] name = "paigasus-helikon-runtime-axum" … release = false` block from `release-plz.toml` (confirm location with `grep -n runtime-axum release-plz.toml`).
- [ ] **Step 3: Workspace pin** — in root `Cargo.toml` `[workspace.dependencies]`, bump `paigasus-helikon-runtime-axum = { path = "…", version = "0.1.0" }`.
- [ ] **Step 4: Facade bump** (the SMA-346 cascade fix) — bump `crates/paigasus-helikon/Cargo.toml` `version` (patch) **and** its own `[workspace.dependencies]` self-pin in root `Cargo.toml`; add a facade CHANGELOG line noting the `runtime-axum` dep bump. (No core bump — no new core API.)
- [ ] **Step 5: Run the full CI gate locally** (the exact commands, not per-crate):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
cargo deny check
mdbook build docs/book
```

Expected: all green. Fix any doc-coverage shortfall by documenting the flagged public items.

- [ ] **Step 6: Commit** (`chore(release): SMA-331 lift stage-1 gates for runtime-axum`) — versions + release-plz + CHANGELOGs in one commit, as CLAUDE.md prescribes.

---

## Self-Review

**Spec coverage** (spec §→task):
- §2/§3 EventLog+replay → Task 3. §4 registry/retention → Task 4. §5 providers/builder/routes → Tasks 5–11. §5 intra-session serialization → Tasks 5 (locks) + 10/12. §6 error/status + cancel scoping → Tasks 2, 10, 12. §7 module layout → Tasks 2–13. §8 testing + doc gate → Tasks 9–13 + 16. §9 deps → Task 1. §10 release + docs → Tasks 14–16. §11 scope boundaries → respected (no durable history, no cross-node, no built-in auth). ✅ No uncovered requirement.
- §5 OpenAPI "static + dynamic agent listing" → Task 13. ✅

**Placeholder scan:** code blocks give concrete signatures/commands; the few prose-described bodies (DTO `from_events` scan, SSE/WS `select!` loops, openapi injection) name exact inputs/outputs and the events to match — acceptable for a skilled implementer, no "TBD"/"handle edge cases". ✅

**Type consistency:** `EventLog`/`RunHandle`/`RunRegistry`/`RunResponse`/`RunRequest`/`AgentInfo`/`SessionProvider`/`ContextProvider`/`AuthLayer`/`AppState` names are used identically across tasks; `subscribe(from)`, `read_from(cursor)`, `note_terminal`/`sweep`, `into_agent_input`, `from_events`, `with_default_context`, `serve_with_listener` match their definitions. ✅

**Ordering:** units (2–8) precede the HTTP harness (9), which precedes the route handlers (10–13); docs/release (14–16) last so nothing publishes mid-build. ✅
