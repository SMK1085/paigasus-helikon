# forkd microVM GC / reconciliation (SMA-447) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an operator-triggered `ForkdBackend::reconcile()` that lists the forkd controller's sandboxes, reaps tag-matching orphans older than a configurable `reap_age`, and returns a `ReconcileReport` — plus repoint the stale orphan-window comment.

**Architecture:** A new inherent async method on the concrete `ForkdBackend` (not on the shared `ExecutionBackend` trait — it is forkd-specific). Orphan detection is **age-only** (reap tag-matching sandboxes strictly older than `reap_age`, default 300s). Deletes fan out with bounded concurrency (8). A `404` on delete is an idempotent success. A new public `build_backend()` finisher returns the concrete backend so `reconcile()` is reachable (the existing `build()` keeps returning `Arc<dyn ExecutionBackend>`).

**Tech Stack:** Rust, `reqwest` (REST client), `tokio` (timeouts), `futures-util` (`join_all`), `serde`, `wiremock` (tests). Feature-gated behind `microvm`.

**Spec:** `docs/superpowers/specs/2026-06-23-sma-447-forkd-microvm-gc-reconciliation-design.md` (read it; this plan implements it verbatim).

## Global Constraints

- **Commit prefix:** `<type>(<scope>): SMA-447 <lowercase subject>` (e.g. `feat(tools): SMA-447 add reconcile()`). Conventional Commits, lowercase subject, enforced by the local `commit-msg` hook (`convco`).
- **Commits are signed** via a 1Password SSH key; if a commit fails with "failed to fill whole buffer", the vault is locked — stop and ask the user to unlock, never bypass signing.
- **Never `git add -A`** (`.env`/`.claude` are untracked-but-not-ignored). Stage explicit paths only; verify with `git show --stat` after committing.
- **Run `cargo fmt --all` and `cargo clippy --workspace --all-features --all-targets -- -D warnings` before every commit** (the pre-commit hook is a deliberate no-op; only pre-push catches these).
- **`missing_docs` is `-D warnings` in the docs job:** every new `pub` item (`reconcile`, `reap_age`, `build_backend`, `ReconcileReport` and each of its fields) needs a `///` doc comment.
- **MSRV 1.85**, edition inherited from workspace. Dual-license `Apache-2.0 OR MIT` (no per-file headers needed).
- **Feature gating:** all new runtime code lives in `crates/paigasus-helikon-tools/src/exec/forkd.rs`, which compiles only under the `microvm` feature. Test with `--features microvm`.
- **Branch:** `feature/sma-447-paigasus-helikon-tools-forkd-microvm-gcreconciliation-of` (already checked out).

---

## File Structure

- `crates/paigasus-helikon-tools/Cargo.toml` — `futures-util` optional dep added to the `microvm` feature.
- `crates/paigasus-helikon-tools/src/exec/forkd.rs` — the feature: consts, `reap_age` plumbing, `build_backend()`, `ReconcileReport`, `SandboxListEntry`, `get_json`/`try_destroy` helpers, `reconcile()`, comment repoint, in-module unit tests.
- `crates/paigasus-helikon-tools/src/exec/mod.rs:37` — re-export `ReconcileReport`.
- `crates/paigasus-helikon-tools/src/lib.rs:46` — re-export `ReconcileReport`.
- `crates/paigasus-helikon-tools/tests/forkd_reconcile.rs` — **new** wiremock integration suite.
- `crates/paigasus-helikon-tools/tests/forkd_live.rs` — `concrete_backend()` helper + `live_forkd_reconcile_is_callable`.
- `crates/paigasus-helikon-tools/README.md` — GC/reconciliation note.
- `docs/book/src/concepts/tools.md` — GC/reconciliation paragraph.
- `docs/runbooks/forkd-live-validation.md` — "Validating reconcile()" section.

---

## Task 1: Builder plumbing — `reap_age` field + `build_backend()` finisher + dep

**Files:**
- Modify: `crates/paigasus-helikon-tools/Cargo.toml` (dependencies + `microvm` feature)
- Modify: `crates/paigasus-helikon-tools/src/exec/forkd.rs` (consts, builder field/method, rename `into_backend`→`build_backend`, `build()`, in-module tests)

**Interfaces:**
- Produces: `pub const`-equivalents `DEFAULT_REAP_AGE: Duration` (300s) and `REAP_CONCURRENCY: usize` (8) (private module consts); `ForkdBackendBuilder::reap_age(Duration) -> Self`; `ForkdBackendBuilder::build_backend() -> Result<ForkdBackend, ForkdError>` (public); a private `reap_age: Duration` field on both `ForkdBackendBuilder` and `ForkdBackend`. Task 2 consumes `self.reap_age` inside `reconcile()`.

- [ ] **Step 1: Add the optional `futures-util` dependency and gate it into `microvm`**

In `crates/paigasus-helikon-tools/Cargo.toml`, add to `[dependencies]` (after the `htmd` line, ~line 27):

```toml
futures-util          = { workspace = true, optional = true }
```

Change the `microvm` feature line (currently line 68) from:

```toml
microvm = ["dep:reqwest", "tokio/net", "tokio/io-util"]
```

to:

```toml
microvm = ["dep:reqwest", "dep:futures-util", "tokio/net", "tokio/io-util"]
```

(Leave the existing `futures-util` `[dev-dependencies]` entry as-is — both can coexist; cargo merges them.)

- [ ] **Step 2: Write the failing in-module test for `reap_age` + `build_backend`**

In `crates/paigasus-helikon-tools/src/exec/forkd.rs`, inside the existing `#[cfg(test)] mod tests { … }` block (after `debug_redacts_the_bearer_token`, before the closing `}`), add:

```rust
#[test]
fn builder_sets_reap_age_and_build_backend_is_public() {
    // Default reap_age is DEFAULT_REAP_AGE.
    let b = ForkdBackend::builder("https://localhost:8080")
        .bearer_token("t")
        .snapshot("s")
        .build_backend()
        .unwrap();
    assert_eq!(b.reap_age, DEFAULT_REAP_AGE);
    // A custom reap_age is carried onto the backend.
    let b2 = ForkdBackend::builder("https://localhost:8080")
        .bearer_token("t")
        .snapshot("s")
        .reap_age(Duration::from_secs(42))
        .build_backend()
        .unwrap();
    assert_eq!(b2.reap_age, Duration::from_secs(42));
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p paigasus-helikon-tools --features microvm builder_sets_reap_age_and_build_backend_is_public`
Expected: FAIL — compile error (`no method named reap_age`, `no method named build_backend`, `no field reap_age`).

- [ ] **Step 4: Add the consts**

In `forkd.rs`, just after the existing `CONTROL_TIMEOUT` const (~line 28), add:

```rust
/// Default minimum age a tag-matching sandbox must reach before [`ForkdBackend::reconcile`]
/// will reap it (10× the default exec timeout). MUST exceed your longest expected run.
const DEFAULT_REAP_AGE: Duration = Duration::from_secs(300);
/// Bounded concurrency for the reconcile reap fan-out (simultaneous in-flight DELETEs).
const REAP_CONCURRENCY: usize = 8;
```

- [ ] **Step 5: Add the `reap_age` field to the builder and its default**

In `ForkdBackendBuilder` (the struct, ~line 95), add a field after `enforce_egress: Option<String>,`:

```rust
    reap_age: Duration,
```

In `ForkdBackend::builder()` (the constructor, ~line 242-253), add to the returned struct literal after `enforce_egress: None,`:

```rust
            reap_age: DEFAULT_REAP_AGE,
```

- [ ] **Step 6: Add the `reap_age()` builder method**

In `impl ForkdBackendBuilder`, after the `enforce_egress` method (~line 158), add:

```rust
    /// Minimum age a tag-matching sandbox must reach before [`ForkdBackend::reconcile`]
    /// will reap it. MUST exceed your longest expected run **plus** any clock skew
    /// between this host and the controller host, or a long legitimate run could be
    /// reaped. Default: 300s (10× the default 30s exec timeout).
    pub fn reap_age(mut self, age: Duration) -> Self {
        self.reap_age = age;
        self
    }
```

- [ ] **Step 7: Add the `reap_age` field to `ForkdBackend` and carry it in the finisher; rename `into_backend` → public `build_backend`**

In the `ForkdBackend` struct (~line 214), add a field after `egress_enforced: bool,`:

```rust
    reap_age: Duration,
```

Rename `fn into_backend` to `pub fn build_backend` and update its doc comment. The method header (~line 160-162) becomes:

```rust
    /// Finish building into the concrete [`ForkdBackend`]. Use this (instead of
    /// [`Self::build`]) when you need to call [`ForkdBackend::reconcile`], which the
    /// `Arc<dyn ExecutionBackend>` returned by `build()` cannot reach. Wrap the result
    /// in an `Arc` once and clone it to a `Arc<dyn ExecutionBackend>` for `BashTool`.
    pub fn build_backend(self) -> Result<ForkdBackend, ForkdError> {
```

In that method's returned `Ok(ForkdBackend { … })` literal, add after `egress_enforced,`:

```rust
            reap_age: self.reap_age,
```

- [ ] **Step 8: Point `build()` at `build_backend()`**

Change the body of `pub fn build` (~line 204-206) from `Ok(Arc::new(self.into_backend()?))` to:

```rust
    pub fn build(self) -> Result<Arc<dyn ExecutionBackend>, ForkdError> {
        Ok(Arc::new(self.build_backend()?))
    }
```

- [ ] **Step 9: Update the existing in-module tests that call `into_backend()`**

In the `#[cfg(test)] mod tests` block, replace every `.into_backend()` with `.build_backend()`. There are five call sites: `guarantees_are_honest`, `guarantees_network_none_without_enforce_egress`, `builder_carries_egress_policy_and_requires_fields` (two calls), `rejects_insecure_remote_http_controller` (three calls), and `debug_redacts_the_bearer_token`. Verify none remain:

Run: `grep -n into_backend crates/paigasus-helikon-tools/src/exec/forkd.rs`
Expected: no output.

- [ ] **Step 10: Run the new + existing in-module tests**

Run: `cargo test -p paigasus-helikon-tools --features microvm --lib`
Expected: PASS — `builder_sets_reap_age_and_build_backend_is_public` and all pre-existing forkd unit tests green.

- [ ] **Step 11: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --features microvm --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/Cargo.toml crates/paigasus-helikon-tools/src/exec/forkd.rs
git commit -m "feat(tools): SMA-447 add reap_age builder option and public build_backend finisher"
git show --stat HEAD
```

---

## Task 2: `reconcile()` + `ReconcileReport` + helpers + re-exports + comment repoint

**Files:**
- Create: `crates/paigasus-helikon-tools/tests/forkd_reconcile.rs`
- Modify: `crates/paigasus-helikon-tools/src/exec/forkd.rs` (types, helpers, `reconcile`, comment, in-module serde test)
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs:37`
- Modify: `crates/paigasus-helikon-tools/src/lib.rs:46`

**Interfaces:**
- Consumes: `self.reap_age`, `DEFAULT_REAP_AGE`, `REAP_CONCURRENCY`, `build_backend()` (Task 1); existing `self.client`, `self.base`, `self.token`, `self.snapshot`, `CONTROL_TIMEOUT`, `destroy()`.
- Produces: `pub struct ReconcileReport { pub scanned: usize, pub reaped: Vec<String>, pub failed: Vec<String>, pub skipped_unageable: usize }`; `pub async fn ForkdBackend::reconcile(&self) -> Result<ReconcileReport, ToolError>`. Re-exported as `paigasus_helikon_tools::ReconcileReport` (and `paigasus_helikon::tools::ReconcileReport` via the facade module alias).

- [ ] **Step 1: Write the failing wiremock integration suite**

Create `crates/paigasus-helikon-tools/tests/forkd_reconcile.rs`:

```rust
#![allow(missing_docs)]
#![cfg(feature = "microvm")]

use std::time::{SystemTime, UNIX_EPOCH};

use paigasus_helikon_tools::ForkdBackend;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[tokio::test]
async fn reconcile_reaps_only_old_tag_matching() {
    let server = MockServer::start().await;
    let now = now_secs();
    let old = now - 600; // older than the 300s default reap_age
    // LIST: old tag-match (reap), young tag-match (keep), old other-tag (keep),
    // old tag-match with NO created_at_unix (skip → skipped_unageable).
    Mock::given(method("GET"))
        .and(path("/v1/sandboxes"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id":"old-match",   "snapshot_tag":"snap-1", "created_at_unix": old},
            {"id":"young-match", "snapshot_tag":"snap-1", "created_at_unix": now},
            {"id":"old-other",   "snapshot_tag":"other",  "created_at_unix": old},
            {"id":"no-ts",       "snapshot_tag":"snap-1"}
        ])))
        .mount(&server)
        .await;
    // Only old-match may be deleted — scoped + expect(1).
    let del = Mock::given(method("DELETE"))
        .and(path("/v1/sandboxes/old-match"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount_as_scoped(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("test-token")
        .snapshot("snap-1")
        .build_backend()
        .expect("builds");
    let report = backend.reconcile().await.expect("reconcile ok");

    assert_eq!(report.scanned, 4);
    assert_eq!(report.reaped, vec!["old-match".to_string()]);
    assert!(report.failed.is_empty(), "no failures: {:?}", report.failed);
    assert_eq!(report.skipped_unageable, 1);
    drop(del); // verifies the DELETE fired exactly once
}

#[tokio::test]
async fn reconcile_list_failure_is_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sandboxes"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("test-token")
        .snapshot("snap-1")
        .build_backend()
        .unwrap();
    let err = backend.reconcile().await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("HTTP 500"), "unexpected error: {msg}");
    assert!(!msg.contains("test-token"), "token leaked: {msg}");
}

#[tokio::test]
async fn reconcile_delete_failure_is_nonfatal() {
    let server = MockServer::start().await;
    let old = now_secs() - 600;
    Mock::given(method("GET"))
        .and(path("/v1/sandboxes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id":"old-match","snapshot_tag":"snap-1","created_at_unix": old}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/sandboxes/old-match"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("t")
        .snapshot("snap-1")
        .build_backend()
        .unwrap();
    let report = backend.reconcile().await.expect("reconcile ok despite delete 500");
    assert_eq!(report.failed, vec!["old-match".to_string()]);
    assert!(report.reaped.is_empty());
}

#[tokio::test]
async fn reconcile_already_gone_is_idempotent() {
    let server = MockServer::start().await;
    let old = now_secs() - 600;
    Mock::given(method("GET"))
        .and(path("/v1/sandboxes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id":"old-match","snapshot_tag":"snap-1","created_at_unix": old}
        ])))
        .mount(&server)
        .await;
    // 404 = already gone → idempotent success.
    Mock::given(method("DELETE"))
        .and(path("/v1/sandboxes/old-match"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("t")
        .snapshot("snap-1")
        .build_backend()
        .unwrap();
    let report = backend.reconcile().await.expect("reconcile ok");
    assert_eq!(report.reaped, vec!["old-match".to_string()]);
    assert!(report.failed.is_empty());
}

#[tokio::test]
async fn reconcile_empty_list_reaps_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/sandboxes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("t")
        .snapshot("snap-1")
        .build_backend()
        .unwrap();
    let report = backend.reconcile().await.expect("reconcile ok");
    assert_eq!(report.scanned, 0);
    assert!(report.reaped.is_empty());
    assert!(report.failed.is_empty());
    assert_eq!(report.skipped_unageable, 0);
}
```

- [ ] **Step 2: Run the suite to verify it fails**

Run: `cargo test -p paigasus-helikon-tools --features microvm --test forkd_reconcile`
Expected: FAIL — compile error (`reconcile` not found, `ReconcileReport` not found).

- [ ] **Step 3: Add `ReconcileReport` and `SandboxListEntry`**

In `forkd.rs`, after the `SandboxInfo` struct (~line 72), add the list-entry struct:

```rust
/// One sandbox in the `GET /v1/sandboxes` list response. Same item shape as the fork
/// response (SMA-416 spike §7) — we read only what reconcile needs and ignore the
/// rest. `created_at_unix` is `Option` so one odd entry can't fail the whole decode;
/// a missing/unparseable timestamp is counted `skipped_unageable` (never reaped).
#[derive(serde::Deserialize)]
struct SandboxListEntry {
    id: String,
    snapshot_tag: String,
    #[serde(default)]
    created_at_unix: Option<u64>,
}
```

After the `ForkdError` enum (or near the other public types, ~after line 56), add the report:

```rust
/// Outcome of a [`ForkdBackend::reconcile`] sweep.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ReconcileReport {
    /// Total sandboxes the controller LIST returned (across *all* snapshot tags) —
    /// observability into host load, independent of the reap set.
    pub scanned: usize,
    /// Ids successfully reaped (DELETE 2xx, or 404 = already gone → idempotent).
    pub reaped: Vec<String>,
    /// Ids that matched and were old enough but whose DELETE errored (non-404).
    /// Best-effort; non-fatal.
    pub failed: Vec<String>,
    /// Tag-matching entries whose `created_at_unix` was absent/unparseable, so they
    /// could not be aged and were **not** reaped. A high value with empty `reaped`
    /// signals the controller's LIST wire shape drifted.
    pub skipped_unageable: usize,
}
```

- [ ] **Step 4: Add the `get_json` helper and the `try_destroy`/`destroy` refactor**

In `impl ForkdBackend`, after `post_json` (~line 286), add a GET twin that mirrors `post_json`'s error mapping verbatim:

```rust
    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, ToolError> {
        // Mirrors post_json: bearer in the header only; error text carries the URL,
        // never the token.
        let resp = self
            .client
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| ToolError::Other(anyhow::anyhow!("forkd request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(ToolError::Other(anyhow::anyhow!(
                "forkd controller returned HTTP {}",
                resp.status().as_u16()
            )));
        }
        resp.json::<T>()
            .await
            .map_err(|e| ToolError::Other(anyhow::anyhow!("forkd response decode failed: {e}")))
    }
```

Replace the existing `destroy` method (~line 319-328) with a `try_destroy` that carries the outcome, plus a thin `destroy` wrapper that preserves the old fire-and-forget behavior:

```rust
    /// DELETE a sandbox, returning the outcome. A `404` is treated as success
    /// (already gone — idempotent under concurrent/repeat sweeps).
    async fn try_destroy(&self, id: &str) -> Result<(), ToolError> {
        let url = format!("{}/v1/sandboxes/{id}", self.base);
        let resp = self
            .client
            .delete(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| ToolError::Other(anyhow::anyhow!("forkd request failed: {e}")))?;
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(ToolError::Other(anyhow::anyhow!(
                "forkd controller returned HTTP {}",
                resp.status().as_u16()
            )))
        }
    }

    async fn destroy(&self, id: &str) {
        // Best-effort teardown; failures here are not surfaced to the model.
        let _ = self.try_destroy(id).await;
    }
```

- [ ] **Step 5: Add `reconcile()`**

In `impl ForkdBackend`, after `destroy` (still inside the inherent `impl`, not the trait impl), add:

```rust
    /// List the controller's sandboxes and reap orphans of this backend's snapshot
    /// tag that are strictly older than [`reap_age`](ForkdBackendBuilder::reap_age).
    ///
    /// Best-effort per sandbox: only a failed LIST returns `Err`; per-sandbox DELETE
    /// failures (non-404) land in [`ReconcileReport::failed`]. Deletes run with
    /// bounded concurrency, so worst-case latency on a degraded controller is about
    /// `CONTROL_TIMEOUT + ceil(N / REAP_CONCURRENCY) * CONTROL_TIMEOUT` for `N`
    /// candidates. Safe under the operator invariant `reap_age > longest run + skew`.
    pub async fn reconcile(&self) -> Result<ReconcileReport, ToolError> {
        let url = format!("{}/v1/sandboxes", self.base);
        let list: Vec<SandboxListEntry> =
            tokio::time::timeout(CONTROL_TIMEOUT, self.get_json(&url))
                .await
                .map_err(|_| ToolError::Other(anyhow::anyhow!("forkd: list timed out")))??;
        let scanned = list.len();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let reap_age_secs = self.reap_age.as_secs();

        let mut skipped_unageable = 0usize;
        let mut candidates: Vec<String> = Vec::new();
        for entry in list {
            if entry.snapshot_tag != self.snapshot {
                continue; // other tag — counts only toward `scanned`
            }
            match entry.created_at_unix {
                None => skipped_unageable += 1,
                Some(t) if now.saturating_sub(t) > reap_age_secs => candidates.push(entry.id),
                Some(_) => {} // young enough — protected
            }
        }

        let mut reaped = Vec::new();
        let mut failed = Vec::new();
        for chunk in candidates.chunks(REAP_CONCURRENCY) {
            let outcomes = futures_util::future::join_all(chunk.iter().map(|id| async move {
                let res = tokio::time::timeout(CONTROL_TIMEOUT, self.try_destroy(id)).await;
                (id.clone(), matches!(res, Ok(Ok(()))))
            }))
            .await;
            for (id, ok) in outcomes {
                if ok {
                    reaped.push(id);
                } else {
                    failed.push(id);
                }
            }
        }

        Ok(ReconcileReport {
            scanned,
            reaped,
            failed,
            skipped_unageable,
        })
    }
```

- [ ] **Step 6: Repoint the stale orphan-window comment**

In the `ExecutionBackend for ForkdBackend` `run()` method, replace the comment at the top of `run` (~lines 394-396) — currently:

```rust
        // Accepted skeleton gap: if the controller commits a fork but we fail to
        // read its id (decode error / client timeout after commit), that sandbox
        // is orphaned — we have no id to DELETE. SMA-437 adds GC/reconciliation.
```

with:

```rust
        // Accepted gap: if the controller commits a fork but we fail to read its id
        // (decode error / client timeout after commit), that sandbox is orphaned — we
        // have no id to DELETE here. It is reaped by the age-based `reconcile()` sweep
        // (SMA-447) once it ages past `reap_age`, provided the controller stamps a
        // parseable `created_at_unix` (otherwise it surfaces as `skipped_unageable`).
```

- [ ] **Step 7: Re-export `ReconcileReport`**

In `crates/paigasus-helikon-tools/src/exec/mod.rs`, change the forkd re-export (line 37) from:

```rust
pub use forkd::{ForkdBackend, ForkdBackendBuilder, ForkdError};
```

to:

```rust
pub use forkd::{ForkdBackend, ForkdBackendBuilder, ForkdError, ReconcileReport};
```

In `crates/paigasus-helikon-tools/src/lib.rs`, change the microvm group (line 46) from:

```rust
pub use exec::{ForkdBackend, ForkdBackendBuilder, ForkdError};
```

to:

```rust
pub use exec::{ForkdBackend, ForkdBackendBuilder, ForkdError, ReconcileReport};
```

(The `/// forkd microVM backend types.` doc comment on line 44 covers the whole `pub use`; `ReconcileReport`'s own definition doc satisfies `missing_docs`.)

- [ ] **Step 8: Add the in-module serde unit test for `SandboxListEntry`**

In the `#[cfg(test)] mod tests` block of `forkd.rs`, after `fork_and_exec_responses_deserialize`, add:

```rust
#[test]
fn sandbox_list_entry_deserializes() {
    // Extra fields (guest_addr, pid, …) are ignored; created_at_unix may be absent.
    let with_ts: SandboxListEntry = serde_json::from_str(
        r#"{"id":"sb-1","snapshot_tag":"t","guest_addr":"10.0.0.2","created_at_unix":1718000000}"#,
    )
    .unwrap();
    assert_eq!(with_ts.id, "sb-1");
    assert_eq!(with_ts.snapshot_tag, "t");
    assert_eq!(with_ts.created_at_unix, Some(1718000000));
    let no_ts: SandboxListEntry =
        serde_json::from_str(r#"{"id":"sb-2","snapshot_tag":"t"}"#).unwrap();
    assert_eq!(no_ts.created_at_unix, None);
}
```

- [ ] **Step 9: Run the full microvm test set**

Run: `cargo test -p paigasus-helikon-tools --features microvm`
Expected: PASS — the five `forkd_reconcile` integration tests, the new serde unit test, and all pre-existing forkd tests green.

- [ ] **Step 10: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --features microvm --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src/exec/forkd.rs \
        crates/paigasus-helikon-tools/src/exec/mod.rs \
        crates/paigasus-helikon-tools/src/lib.rs \
        crates/paigasus-helikon-tools/tests/forkd_reconcile.rs
git commit -m "feat(tools): SMA-447 reap orphaned forkd microVMs via reconcile()"
git show --stat HEAD
```

---

## Task 3: Live test — `live_forkd_reconcile_is_callable`

**Files:**
- Modify: `crates/paigasus-helikon-tools/tests/forkd_live.rs`

**Interfaces:**
- Consumes: `ForkdBackend::build_backend()` (Task 1), `ForkdBackend::reconcile()` (Task 2), the existing `live_env()` helper.

- [ ] **Step 1: Add a concrete-backend helper**

In `crates/paigasus-helikon-tools/tests/forkd_live.rs`, after the existing `backend(enforce: bool)` helper (~line 54), add:

```rust
/// Build the concrete `ForkdBackend` (not `Arc<dyn>`) so `reconcile()` is reachable.
fn concrete_backend() -> Option<ForkdBackend> {
    let (url, token, snapshot) = live_env()?;
    let mut b = ForkdBackend::builder(url)
        .bearer_token(token)
        .snapshot(snapshot);
    if let Ok(ca_path) = std::env::var("FORKD_CA") {
        b = b.controller_ca(std::fs::read(ca_path).expect("FORKD_CA file readable"));
    }
    Some(b.build_backend().expect("backend builds"))
}
```

- [ ] **Step 2: Add the env-gated live test**

At the end of the file, add:

```rust
#[tokio::test]
async fn live_forkd_reconcile_is_callable() {
    let Some(backend) = concrete_backend() else {
        return;
    };
    // Prime one real run (it self-destructs) so the controller has handled our tag.
    let _ = backend.run(ExecRequest::new("echo prime")).await;
    let report = backend
        .reconcile()
        .await
        .expect("reconcile succeeds against a live controller");
    // Loud, inspectable record — true orphan-injection + created_at_unix contract
    // verification are the manual runbook step (forkd-live-validation.md).
    eprintln!(
        "live reconcile: scanned={} reaped={:?} failed={:?} skipped_unageable={}",
        report.scanned, report.reaped, report.failed, report.skipped_unageable
    );
}
```

(`ForkdBackend` is already imported at the top of `forkd_live.rs`; if the compiler reports it unused-until-now, it is now used by `concrete_backend`'s return type — no import change needed.)

- [ ] **Step 3: Verify it compiles and loud-skips without env**

Run: `cargo test -p paigasus-helikon-tools --features microvm --test forkd_live`
Expected: PASS — `live_forkd_reconcile_is_callable` compiles and returns early (no `FORKD_URL` set), the other live tests loud-skip as before.

- [ ] **Step 4: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --features microvm --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/tests/forkd_live.rs
git commit -m "test(tools): SMA-447 add env-gated live reconcile() smoke test"
git show --stat HEAD
```

---

## Task 4: Documentation — README + book + runbook

**Files:**
- Modify: `crates/paigasus-helikon-tools/README.md`
- Modify: `docs/book/src/concepts/tools.md`
- Modify: `docs/runbooks/forkd-live-validation.md`

**Interfaces:** none (docs only). Mirror the existing tools-README rust fence style (plain ```rust fragments — the tools README is **not** compiled as doctests; only the facade README is, per SMA-424).

- [ ] **Step 1: README — add a GC/reconciliation subsection**

In `crates/paigasus-helikon-tools/README.md`, after the "microVM egress enforcement" section (after the line about `docs/runbooks/forkd-live-validation.md`, ~line 39), add:

```markdown
### microVM GC / reconciliation (SMA-447)

A forked microVM leaks if the controller commits it but the client never learns its
id (a decode error, or a client-side timeout firing *after* the fork commits). To
reap such orphans, build the **concrete** backend with `build_backend()`, then call
`reconcile()` from your own scheduler / shutdown hook:

```rust
use std::sync::Arc;
use paigasus_helikon_tools::{ExecutionBackend, ForkdBackend};

let backend: Arc<ForkdBackend> = Arc::new(
    ForkdBackend::builder("https://controller:8889")
        .bearer_token(token)
        .snapshot("agent-base")
        .reap_age(std::time::Duration::from_secs(600)) // > your longest run + clock skew
        .build_backend()?,
);
let shared: Arc<dyn ExecutionBackend> = backend.clone(); // hand to BashTool

// later, periodically:
let report = backend.reconcile().await?;
// report.scanned / reaped / failed / skipped_unageable
```

`reconcile()` lists `GET /v1/sandboxes`, reaps sandboxes of this backend's
`snapshot` tag strictly older than `reap_age` (default 300s), and is best-effort —
only a failed LIST is an error. **Invariant:** set `reap_age` above your longest
expected run plus any clock skew, or a long legitimate run could be reaped.
```

(Note: the closing ```` ``` ```` of the inner rust block must be preserved — the snippet above contains a nested fenced block.)

- [ ] **Step 2: book — add a GC/reconciliation paragraph**

In `docs/book/src/concepts/tools.md`, in the `ForkdBackend` section, after the network-containment "layered model" paragraph (~after line 244), add:

```markdown
**GC / reconciliation (SMA-447).** A fork that commits a microVM on the controller
but whose id the client never learns (a decode error, or a client-side timeout after
commit) leaks that VM — there is no id to `DELETE`. Build the concrete backend with
`build_backend()` (instead of `build()`) and call `reconcile()` from your own
scheduler or shutdown hook: it lists the controller's sandboxes and reaps the ones of
this backend's snapshot tag that are strictly older than `reap_age` (builder option,
default 300s), returning a `ReconcileReport { scanned, reaped, failed,
skipped_unageable }`. Detection is age-only and stateless, so it is safe across
multiple SDK processes sharing one controller — but you **must** set `reap_age` above
your longest expected run plus clock skew, or a still-running command could be reaped.
```

- [ ] **Step 3: runbook — add a "Validating reconcile()" section**

In `docs/runbooks/forkd-live-validation.md`, append a new section at the end:

```markdown
## Validating reconcile() (SMA-447)

`reconcile()` is exercised as a smoke test by `live_forkd_reconcile_is_callable`
(env-gated, runs with `FORKD_URL`/`FORKD_TOKEN`/`FORKD_SNAPSHOT` like the other live
tests). Two things still need a human eye on a live controller:

1. **Wire-contract check (BLOCKER-1).** The age filter depends on the LIST item
   carrying `created_at_unix` as integer **seconds**. Capture a real body and confirm:

   ```bash
   curl -sS -H "Authorization: Bearer $FORKD_TOKEN" "$FORKD_URL/v1/sandboxes" | jq '.[0]'
   ```

   If the timestamp field is named differently, or is a float / milliseconds / RFC3339
   string, `reconcile()` will report every entry as `skipped_unageable` and reap
   nothing. Adjust `SandboxListEntry::created_at_unix` (name / type) before relying on
   GC in production.

2. **Orphan-injection check.** Prove a real orphan is reaped:

   ```bash
   # Create a sandbox out-of-band and do NOT destroy it:
   curl -sS -H "Authorization: Bearer $FORKD_TOKEN" -H 'content-type: application/json' \
     -d '{"snapshot_tag":"'"$FORKD_SNAPSHOT"'","n":1,"per_child_netns":true}' \
     "$FORKD_URL/v1/sandboxes"
   # Wait > reap_age, then run a reconcile built with a short reap_age (e.g. 1s) and
   # confirm the leaked id appears in `reaped`. forkd ls should then show it gone.
   ```
```

- [ ] **Step 4: Verify the book still builds clean**

Run: `mdbook build docs/book`
Expected: success, no linkcheck warnings (the workflow treats them as errors).

(If `mdbook` is not installed: `cargo install mdbook mdbook-linkcheck` — or note that CI's `book-build` job is the gate and proceed.)

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-tools/README.md \
        docs/book/src/concepts/tools.md \
        docs/runbooks/forkd-live-validation.md
git commit -m "docs(tools): SMA-447 document reconcile()/GC in README, book, runbook"
git show --stat HEAD
```

---

## Task 5: Full local CI-gate verification

**Files:** none (verification only).

- [ ] **Step 1: Run the exact CI gates locally**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
mdbook build docs/book
```

Expected: every command exits 0. `cargo doc` must not warn (validates the new `///` docs + intra-doc links). If `cargo test --workspace --all-features` is slow, the targeted `cargo test -p paigasus-helikon-tools --all-features` plus the workspace build is an acceptable interim, but the full workspace test must pass before the PR.

- [ ] **Step 2: Confirm the diff matches the plan and the spec**

```bash
git log --oneline dfab017..HEAD
git diff --stat dfab017..HEAD
grep -rn into_backend crates/paigasus-helikon-tools/src   # expected: no output
```

Expected: four feature/test/docs commits (Tasks 1-4) on top of the design+spec commits; the file set matches §15 of the spec; no `into_backend` references remain.

---

## Self-Review (completed during planning)

- **Spec coverage:** D1 manual trigger → Task 2 `reconcile()`. D2 age-only → Task 2 step 5 partition logic. D3/D4 deferrals → not implemented (correct). `reap_age` + invariant → Task 1 + docs (Task 4). `build_backend()` cliff fix → Task 1 + README/book pattern (Task 4). `ReconcileReport` + `skipped_unageable` → Task 2 step 3. Strict `>` → Task 2 step 5. Bounded concurrency + `futures-util` → Task 1 step 1 + Task 2 step 5. Idempotent 404 → Task 2 step 4. `get_json` error parity → Task 2 step 4. Comment repoint → Task 2 step 6. Re-exports → Task 2 step 7. Tests (wiremock + serde + live) → Tasks 2-3. Docs → Task 4. Release (no core bump) → no version edits in any task (release-plz handles it). Every spec section maps to a task.
- **Placeholder scan:** no TBD/TODO; every code step shows complete code.
- **Type consistency:** `ReconcileReport { scanned, reaped, failed, skipped_unageable }`, `SandboxListEntry { id, snapshot_tag, created_at_unix }`, `build_backend()`, `reap_age()`, `try_destroy()`, `get_json()`, `reconcile()`, `DEFAULT_REAP_AGE`, `REAP_CONCURRENCY` — names identical across Tasks 1-4 and the tests.
