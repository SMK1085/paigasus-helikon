# SMA-416 — forkd microVM `ExecutionBackend` (spike + skeleton) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a feature-gated, mock-tested `ForkdBackend` skeleton (a REST client of the forkd Firecracker controller) as the microVM tier behind the existing `ExecutionBackend` trait, plus the spike note — without running a real KVM VM.

**Architecture:** `ForkdBackend` is a portable HTTP client (reqwest + rustls + bearer) that drives forkd's controller REST API fork→exec→destroy. It plugs into the SMA-413 `ExecutionBackend` trait with no change to `BashTool`. A new `Isolation::Virtualized` variant and honest `guarantees()` (`network: None` — egress unenforced in the skeleton) describe it. Egress is *carried* as an `EgressPolicy` config but not enforced (deferred to SMA-437).

**Tech Stack:** Rust, `async-trait`, `reqwest` 0.13 (json + rustls), `serde`, `thiserror`, `tokio`; tests via `wiremock`. Feature-gated behind a new `microvm` Cargo feature (portable — NOT target-gated).

**Reference spec:** `docs/superpowers/specs/2026-06-21-sma-416-forkd-microvm-design.md`

---

## File structure

| File | Responsibility | Action |
|------|----------------|--------|
| `docs/superpowers/specs/2026-06-21-sma-416-forkd-microvm-spike.md` | The spike note (1st AC) | Create (Task 1) |
| `crates/paigasus-helikon-tools/src/exec/mod.rs` | Add `Isolation::Virtualized`; gate + re-export `forkd` | Modify (Tasks 2, 3, 4, 5) |
| `crates/paigasus-helikon-tools/src/exec/forkd.rs` | `EgressPolicy`, `ForkdError`, REST types, `ForkdBackend` + builder | Create (Tasks 3–6) |
| `crates/paigasus-helikon-tools/src/lib.rs` | Re-export the `microvm` public surface | Modify (Tasks 3, 4, 5) |
| `crates/paigasus-helikon-tools/Cargo.toml` | `microvm` feature | Modify (Task 3) |
| `crates/paigasus-helikon/Cargo.toml` | Facade `tools-microvm` passthrough | Modify (Task 3) |
| `crates/paigasus-helikon-tools/tests/exec_backend.rs` | `Isolation::Virtualized` test | Modify (Task 2) |
| `crates/paigasus-helikon-tools/tests/forkd_backend.rs` | wiremock integration tests | Create (Tasks 5, 6) |
| `docs/book/src/concepts/tools.md` | mdBook microVM tier + honesty caveat | Modify (Task 7) |
| `README.md`, `crates/paigasus-helikon/README.md`, `crates/paigasus-helikon-tools/README.md` | Feature maps | Modify (Task 7) |

**Do NOT manually bump versions or edit CHANGELOGs.** `-tools` and the facade are already-released; release-plz auto-bumps both from the conventional commits on merge (additive `feat` → 0.x patch). Manual bumps would defeat the cascade.

**Confirmed forkd controller REST contract** (Task 1 verified this against forkd's `docs/API.md`, v0.5.2, and it was double-checked via WebFetch — the resource is `/v1/sandboxes`, exec takes `args: string[]` + `timeout_secs`, there is **no per-exec `env` field**, and fork returns an **array**):

| Step | Method + path | Request JSON | Response JSON |
|------|---------------|--------------|---------------|
| Fork | `POST {base}/v1/sandboxes` | `{"snapshot_tag":"<tag>","n":1,"per_child_netns":true}` | `[{"id":"sb-…", …}]` (array; take `[0]`) |
| Exec | `POST {base}/v1/sandboxes/{id}/exec` | `{"args":["sh","-c","<cmd>"],"timeout_secs":<N>}` | `{"stdout":"…","stderr":"…","exit_code":0}` |
| Destroy | `DELETE {base}/v1/sandboxes/{id}` | — | `204 No Content` |

All requests carry `Authorization: Bearer <token>` (every route except `/healthz`).

**No `env` injection:** forkd's exec endpoint has no env field — env is a snapshot-boot concern (documented in the spike note), so the skeleton does **not** carry an `env_allowlist`. The wall-clock command timeout is sent as `timeout_secs` (daemon-side) **and** enforced client-side via `tokio::time::timeout` (defense in depth).

---

## Task 1: Spike note (1st AC — research + document)

**Files:**
- Create: `docs/superpowers/specs/2026-06-21-sma-416-forkd-microvm-spike.md`

This task is research + writing (no code). It needs web access.

- [ ] **Step 1: Research forkd**

Use WebSearch / WebFetch to confirm, from forkd's own repo/docs (`github.com/deeplethe/forkd`):
- License is **Apache-2.0** (fits the cargo-deny allowlist — Apache-2.0 already permitted).
- The controller REST surface: fork / exec-in-guest / destroy endpoints, bearer + rustls auth, Unix/TCP transport. Record the **actual** endpoint paths + JSON shapes.
- Risk profile: pre-1.0 API churn, single-host, `memory.max`-only quota, path-traversal CVE fixed in 0.1.3, per-child netns + vmgenid RNG reseed.

If any claim can't be verified, record that explicitly and proceed against the **assumed contract** in this plan (the skeleton is mock-tested regardless).

- [ ] **Step 2: Write the spike note**

Create the file with these sections (prose, drawing on the spec §2, §3.4, §5):
1. **Viability** — forkd exists / license / API confirmed (or what couldn't be confirmed; E2B named as the fallback controller).
2. **Integration decision: REST, not embed** — rationale (no KVM/VMM crates in our build; mirrors `web/`; alpha VMM behind a process boundary; E2B sibling). Record the **portability departure** from the ticket's "Linux/KVM/x86_64 only" (the REST client is not compile-gated).
3. **Risk assessment** — the list from Step 1 + mitigations (REST boundary + trait seam keep it swappable; pin a known-good version).
4. **Snapshot (guest image) contract** — guest must boot a Linux userland with `/bin/sh` + coreutils; the operator warms/snapshots it out of band; exec runs inside the booted guest with the env allowlist forwarded; **CoW shares warmed state across forks → never bake secrets into the snapshot** (RNG is reseeded per child, so only static secrets are the concern).
5. **Controller TLS trust** — rustls rejects self-signed by default; localhost needs a CA/pin via `.controller_ca`, remote needs a real CA; we deliberately do not expose `danger_accept_invalid_certs`.
6. **Egress approach** — layered (per-VM netns default-deny + CONNECT proxy reusing the promoted SMA-412 domain policy); enforcement deferred to SMA-437; skeleton carries `EgressPolicy`.
7. **Confirmed/assumed API contract** — the endpoint table the skeleton codes against.

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-06-21-sma-416-forkd-microvm-spike.md
git commit -m "docs(spec): SMA-416 add forkd microVM spike note"
```

---

## Task 2: Add `Isolation::Virtualized`

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs` (the `Isolation` enum)
- Test: `crates/paigasus-helikon-tools/tests/exec_backend.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/paigasus-helikon-tools/tests/exec_backend.rs`:

```rust
#[test]
fn isolation_has_virtualized_variant() {
    // Virtualized is a distinct, stronger tier than OsKernel.
    let g = SandboxGuarantees::new(
        Isolation::Virtualized,
        Isolation::None,
        Isolation::Virtualized,
        "vm",
    );
    assert_eq!(g.filesystem, Isolation::Virtualized);
    assert_eq!(g.syscalls, Isolation::Virtualized);
    assert_ne!(Isolation::Virtualized, Isolation::OsKernel);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-tools --test exec_backend isolation_has_virtualized_variant`
Expected: FAIL — `no variant named Virtualized found for enum Isolation`.

- [ ] **Step 3: Add the variant**

In `crates/paigasus-helikon-tools/src/exec/mod.rs`, in `enum Isolation`, after the `OsKernel` variant add:

```rust
    /// Isolated by a hardware-virtualization (KVM/hypervisor) boundary — a
    /// separate guest kernel. `Virtualized` means the whole machine is isolated,
    /// **not** that any one axis is filtered: a microVM does not filter syscalls
    /// the way `OsKernel` (seccomp) does — the guest issues syscalls to its own
    /// kernel. Stronger overall than `OsKernel`, but read each axis as "behind a
    /// VM boundary," not "restricted by an allowlist."
    Virtualized,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p paigasus-helikon-tools --test exec_backend isolation_has_virtualized_variant`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-tools/src/exec/mod.rs crates/paigasus-helikon-tools/tests/exec_backend.rs
git commit -m "feat(tools): SMA-416 add Isolation::Virtualized tier"
```

---

## Task 3: Wire the `microvm` feature + `EgressPolicy`

**Files:**
- Modify: `crates/paigasus-helikon-tools/Cargo.toml` (feature)
- Modify: `crates/paigasus-helikon/Cargo.toml` (facade passthrough)
- Create: `crates/paigasus-helikon-tools/src/exec/forkd.rs`
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs` (gate + re-export)
- Modify: `crates/paigasus-helikon-tools/src/lib.rs` (re-export)

- [ ] **Step 1: Write the failing test**

Create `crates/paigasus-helikon-tools/src/exec/forkd.rs` with ONLY the `EgressPolicy` test module for now (the impl comes in Step 3):

```rust
//! [`ForkdBackend`] — the microVM execution tier: a portable REST client of the
//! forkd Firecracker controller. Feature-gated behind `microvm`. **Experimental
//! skeleton** (SMA-416): the fork→exec→destroy flow is real but the live KVM run
//! and egress *enforcement* are deferred to SMA-437; `guarantees().network` is
//! honestly `None`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egress_policy_deny_all_then_allowlist() {
        let p = EgressPolicy::deny_all().allow_domains(["pypi.org"]);
        assert!(p.is_allowed("pypi.org"));
        assert!(p.is_allowed("files.pypi.org")); // sub-domain
        assert!(!p.is_allowed("evil.test")); // not on the allow-list
    }

    #[test]
    fn egress_policy_deny_beats_allow_and_default_allows() {
        let p = EgressPolicy::allow_all().deny_domains(["evil.test"]);
        assert!(!p.is_allowed("evil.test"));
        assert!(!p.is_allowed("api.evil.test")); // sub-domain
        assert!(p.is_allowed("good.test")); // no allow-list -> default allow
    }
}
```

- [ ] **Step 2: Add the feature wiring so the module compiles**

In `crates/paigasus-helikon-tools/Cargo.toml`, under `[features]` after the `os-sandbox` line add:

```toml
# microVM Bash containment (forkd Firecracker controller, REST client). Off by
# default. Portable: the client compiles everywhere; the daemon needs Linux/KVM.
microvm = ["dep:reqwest"]
```

In `crates/paigasus-helikon-tools/src/exec/mod.rs`, after the `os_sandbox_seatbelt` cfg block, add:

```rust
#[cfg(feature = "microvm")]
mod forkd;
#[cfg(feature = "microvm")]
pub use forkd::EgressPolicy;
```

In `crates/paigasus-helikon-tools/src/lib.rs`, after the `os-sandbox` macos `pub use` block (line ~49) add:

```rust
#[cfg(feature = "microvm")]
pub use exec::EgressPolicy;
```

In `crates/paigasus-helikon/Cargo.toml`, under `[features]` after `tools-os-sandbox` add:

```toml
tools-microvm      = ["tools", "paigasus-helikon-tools/microvm"]
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p paigasus-helikon-tools --features microvm --lib forkd::tests`
Expected: FAIL — `cannot find type EgressPolicy in this scope` / `EgressPolicy not found`.

- [ ] **Step 4: Implement `EgressPolicy`**

In `crates/paigasus-helikon-tools/src/exec/forkd.rs`, above the `#[cfg(test)] mod tests`, add:

```rust
/// Domain allow/deny config the backend **carries**. The skeleton does not yet
/// *enforce* egress (the netns + CONNECT-proxy layers are SMA-437); this type is
/// the seam that follow-up enforces, and the future cloud sibling shares.
///
/// Matching is sub-domain-aware, case-insensitive, and trailing-dot-insensitive:
/// `example.com` matches `example.com` and `api.example.com`.
#[derive(Debug, Clone, Default)]
pub struct EgressPolicy {
    allow: Option<Vec<String>>,
    deny: Vec<String>,
}

impl EgressPolicy {
    /// Deny all egress (an empty allow-list permits nothing).
    pub fn deny_all() -> Self {
        Self {
            allow: Some(Vec::new()),
            deny: Vec::new(),
        }
    }

    /// Allow all egress (no allow-list and no deny-list).
    pub fn allow_all() -> Self {
        Self::default()
    }

    /// Add allowed domains. Setting any allow-list switches the policy to
    /// default-deny (only listed domains and their sub-domains are permitted).
    pub fn allow_domains<I, S>(mut self, domains: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allow
            .get_or_insert_with(Vec::new)
            .extend(domains.into_iter().map(Into::into));
        self
    }

    /// Add denied domains. A deny match always refuses, beating any allow.
    pub fn deny_domains<I, S>(mut self, domains: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.deny.extend(domains.into_iter().map(Into::into));
        self
    }

    /// `true` if `host` is permitted: not denied, and — when an allow-list is set
    /// — matching it (itself or a sub-domain).
    pub fn is_allowed(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let matches = |entry: &String| {
            let e = entry.trim_end_matches('.').to_ascii_lowercase();
            host == e || host.ends_with(&format!(".{e}"))
        };
        if self.deny.iter().any(matches) {
            return false;
        }
        match &self.allow {
            Some(list) => list.iter().any(matches),
            None => true,
        }
    }
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p paigasus-helikon-tools --features microvm --lib forkd::tests`
Expected: PASS (2 tests).

- [ ] **Step 6: Verify the feature compiles clean (lib only, no dev-deps)**

Run: `cargo build -p paigasus-helikon-tools --features microvm`
Expected: builds with no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-tools/Cargo.toml crates/paigasus-helikon/Cargo.toml crates/paigasus-helikon-tools/src/exec/forkd.rs crates/paigasus-helikon-tools/src/exec/mod.rs crates/paigasus-helikon-tools/src/lib.rs
git commit -m "feat(tools): SMA-416 add microvm feature + EgressPolicy seam"
```

---

## Task 4: `ForkdError` + REST payload types

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/exec/forkd.rs`
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs` (re-export `ForkdError`)
- Modify: `crates/paigasus-helikon-tools/src/lib.rs` (re-export `ForkdError`)

- [ ] **Step 1: Write the failing tests**

In `crates/paigasus-helikon-tools/src/exec/forkd.rs`, inside `mod tests`, add:

```rust
    #[test]
    fn fork_and_exec_responses_deserialize() {
        // Fork returns an ARRAY of sandboxes (even for n:1); we take the first.
        // Unknown fields (snapshot_tag, …) are ignored.
        let v: Vec<SandboxInfo> =
            serde_json::from_str(r#"[{"id":"sb-9","snapshot_tag":"t"}]"#).unwrap();
        assert_eq!(v[0].id, "sb-9");
        // exit_code may be absent (killed by signal) -> None.
        let e: ExecResp =
            serde_json::from_str(r#"{"stdout":"hi","stderr":"","exit_code":0}"#).unwrap();
        assert_eq!(e.stdout, "hi");
        assert_eq!(e.exit_code, Some(0));
        let e2: ExecResp = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(e2.stdout, "");
        assert_eq!(e2.exit_code, None);
    }

    #[test]
    fn forkd_error_never_embeds_a_token() {
        // Construction errors must be safe to log: no auth material in Display.
        let e = ForkdError::MissingConfig("bearer_token");
        let s = e.to_string();
        assert!(s.contains("bearer_token"));
        assert!(!s.to_lowercase().contains("secret"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p paigasus-helikon-tools --features microvm --lib forkd::tests`
Expected: FAIL — `SandboxInfo` / `ExecResp` / `ForkdError` not found.

- [ ] **Step 3: Implement the types**

In `crates/paigasus-helikon-tools/src/exec/forkd.rs`, at the top (below the module doc), add the imports and types:

```rust
/// Construction-time failures for [`ForkdBackend`]. Runtime failures (daemon
/// unreachable, fork/exec error) surface as `ToolError::Other` from `run`.
///
/// Variants never embed the bearer token — keep auth material out of error text.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ForkdError {
    /// The controller URL could not be parsed.
    #[error("invalid forkd controller URL: {0}")]
    InvalidUrl(String),
    /// A required field (bearer token / snapshot tag) was not set.
    #[error("missing required forkd config: {0}")]
    MissingConfig(&'static str),
    /// The controller CA PEM could not be parsed.
    #[error("invalid controller CA certificate")]
    InvalidCa,
    /// The reqwest client could not be constructed.
    #[error("failed to build forkd HTTP client")]
    ClientBuild,
}

/// `POST /v1/sandboxes` request body — fork `n` children (we use 1) copy-on-write
/// from a warmed snapshot, each in its own network namespace.
#[derive(serde::Serialize)]
struct ForkReq<'a> {
    snapshot_tag: &'a str,
    n: u32,
    per_child_netns: bool,
}

/// One sandbox in the `POST /v1/sandboxes` response **array**. forkd returns more
/// fields (snapshot_tag, guest_addr, …); only `id` is needed, the rest are ignored.
#[derive(serde::Deserialize)]
struct SandboxInfo {
    id: String,
}

/// `POST /v1/sandboxes/{id}/exec` request body. `args` runs verbatim in the guest
/// (no shell expansion), so a shell command is wrapped as `["sh","-c","<cmd>"]`.
/// `timeout_secs` is the daemon-side cap (we also enforce one client-side).
#[derive(serde::Serialize)]
struct ExecReq<'a> {
    args: [&'a str; 3],
    timeout_secs: u64,
}

/// `POST /v1/sandboxes/{id}/exec` response — captured guest output.
#[derive(serde::Deserialize)]
struct ExecResp {
    #[serde(default)]
    stdout: String,
    #[serde(default)]
    stderr: String,
    #[serde(default)]
    exit_code: Option<i32>,
}
```

In `crates/paigasus-helikon-tools/src/exec/mod.rs`, extend the microvm re-export:

```rust
#[cfg(feature = "microvm")]
pub use forkd::{EgressPolicy, ForkdError};
```

In `crates/paigasus-helikon-tools/src/lib.rs`, extend:

```rust
#[cfg(feature = "microvm")]
pub use exec::{EgressPolicy, ForkdError};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p paigasus-helikon-tools --features microvm --lib forkd::tests`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-tools/src/exec/forkd.rs crates/paigasus-helikon-tools/src/exec/mod.rs crates/paigasus-helikon-tools/src/lib.rs
git commit -m "feat(tools): SMA-416 add ForkdError + forkd REST payload types"
```

---

## Task 5: `ForkdBackend` + builder + `run` (happy path)

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/exec/forkd.rs`
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs` (re-export the backend)
- Modify: `crates/paigasus-helikon-tools/src/lib.rs` (re-export the backend)
- Create: `crates/paigasus-helikon-tools/tests/forkd_backend.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/paigasus-helikon-tools/tests/forkd_backend.rs`:

```rust
#![cfg(feature = "microvm")]
#![allow(missing_docs)]

use paigasus_helikon_tools::{ExecRequest, ExecutionBackend, ForkdBackend};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn forks_execs_and_destroys() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/sandboxes"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{"id":"sb-1"}])),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/sandboxes/sb-1/exec"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"stdout":"hello\n","stderr":"","exit_code":0}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/sandboxes/sb-1"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("test-token")
        .snapshot("snap-1")
        .build()
        .expect("builds");
    let out = backend
        .run(ExecRequest::new("echo hello"))
        .await
        .expect("runs");
    assert_eq!(out.stdout, "hello\n");
    assert_eq!(out.exit_code, Some(0));
    assert!(!out.timed_out);
    assert!(!out.truncated);
}
```

Also add the white-box unit tests to `forkd.rs`'s `mod tests`:

```rust
    #[test]
    fn guarantees_are_honest() {
        let b = ForkdBackend::builder("https://localhost:8080")
            .bearer_token("t")
            .snapshot("s")
            .into_backend()
            .unwrap();
        let g = b.guarantees();
        assert_eq!(g.filesystem, Isolation::Virtualized);
        assert_eq!(g.syscalls, Isolation::Virtualized);
        assert_eq!(g.network, Isolation::None); // egress NOT enforced in the skeleton
        assert!(g.label.contains("experimental"));
    }

    #[test]
    fn builder_carries_egress_policy_and_requires_fields() {
        // Missing snapshot -> construction error.
        let err = ForkdBackend::builder("https://localhost:8080")
            .bearer_token("t")
            .into_backend()
            .unwrap_err();
        assert!(matches!(err, ForkdError::MissingConfig("snapshot")));
        // The configured policy is carried on the backend.
        let b = ForkdBackend::builder("https://localhost:8080")
            .bearer_token("t")
            .snapshot("s")
            .egress_policy(EgressPolicy::deny_all().allow_domains(["pypi.org"]))
            .into_backend()
            .unwrap();
        assert!(b.egress.is_allowed("pypi.org"));
        assert!(!b.egress.is_allowed("evil.test"));
    }
```

The unit tests reference `Isolation`, `ForkdBackend`, `EgressPolicy`, and the
`ExecutionBackend` trait — all already brought in by the existing `use super::*;`
at the top of `mod tests` once Step 3 adds `Isolation`/`ExecutionBackend` to
`forkd.rs`'s own `use super::{…}`. Do **not** add a second explicit `use` for
`Isolation` — that collides with the glob and fails `-D warnings`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p paigasus-helikon-tools --features microvm`
Expected: FAIL — `ForkdBackend` / `into_backend` / `guarantees` not found.

- [ ] **Step 3: Implement the backend**

In `crates/paigasus-helikon-tools/src/exec/forkd.rs`, add these imports at the top of the file (below the `//!` module doc, alongside the Task-4 types):

```rust
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use paigasus_helikon_core::ToolError;

use super::{
    ExecOutput, ExecRequest, ExecutionBackend, Isolation, SandboxGuarantees, DEFAULT_MAX_OUTPUT,
    DEFAULT_TIMEOUT,
};

const DEFAULT_UA: &str = concat!("paigasus-helikon-tools/", env!("CARGO_PKG_VERSION"));
/// Fixed control-plane timeout for the destroy call (the command timeout governs exec).
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
```

Then add the builder and backend:

```rust
/// Builder for [`ForkdBackend`].
pub struct ForkdBackendBuilder {
    controller_url: String,
    bearer_token: Option<String>,
    controller_ca: Option<Vec<u8>>,
    snapshot: Option<String>,
    timeout: Duration,
    max_output_bytes: usize,
    egress: EgressPolicy,
}

impl ForkdBackendBuilder {
    /// Bearer token presented to the controller (required).
    pub fn bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer_token = Some(token.into());
        self
    }
    /// PEM trust root / cert pin for the controller's TLS (required for a
    /// self-signed localhost daemon; use a real CA for a remote host).
    pub fn controller_ca(mut self, pem: impl Into<Vec<u8>>) -> Self {
        self.controller_ca = Some(pem.into());
        self
    }
    /// Warmed parent snapshot tag to fork children from (required; forkd's
    /// `snapshot_tag`).
    pub fn snapshot(mut self, tag: impl Into<String>) -> Self {
        self.snapshot = Some(tag.into());
        self
    }
    /// Wall-clock timeout for the exec step (default 30s).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    /// Truncate captured stdout/stderr to this many bytes each (default 1 MiB).
    pub fn max_output_bytes(mut self, n: usize) -> Self {
        self.max_output_bytes = n;
        self
    }
    /// Egress policy the backend carries (enforcement is SMA-437).
    pub fn egress_policy(mut self, policy: EgressPolicy) -> Self {
        self.egress = policy;
        self
    }

    fn into_backend(self) -> Result<ForkdBackend, ForkdError> {
        // Validate the controller URL up front (parsed value is discarded).
        reqwest::Url::parse(&self.controller_url)
            .map_err(|_| ForkdError::InvalidUrl(self.controller_url.clone()))?;
        let token = self.bearer_token.ok_or(ForkdError::MissingConfig("bearer_token"))?;
        let snapshot = self.snapshot.ok_or(ForkdError::MissingConfig("snapshot"))?;
        let mut cb = reqwest::Client::builder()
            .user_agent(DEFAULT_UA)
            .connect_timeout(CONTROL_TIMEOUT);
        if let Some(pem) = &self.controller_ca {
            let cert = reqwest::Certificate::from_pem(pem).map_err(|_| ForkdError::InvalidCa)?;
            cb = cb.add_root_certificate(cert);
        }
        let client = cb.build().map_err(|_| ForkdError::ClientBuild)?;
        Ok(ForkdBackend {
            client,
            base: self.controller_url.trim_end_matches('/').to_string(),
            token,
            snapshot,
            timeout: self.timeout,
            max_output_bytes: self.max_output_bytes,
            egress: self.egress,
        })
    }

    /// Finish building into a shareable `Arc<dyn ExecutionBackend>`.
    pub fn build(self) -> Result<Arc<dyn ExecutionBackend>, ForkdError> {
        Ok(Arc::new(self.into_backend()?))
    }
}

/// The microVM execution backend — a REST client of the forkd controller. See
/// the module docs: experimental skeleton; egress is carried but not enforced.
pub struct ForkdBackend {
    client: reqwest::Client,
    base: String,
    token: String,
    snapshot: String,
    timeout: Duration,
    max_output_bytes: usize,
    egress: EgressPolicy,
}

impl ForkdBackend {
    /// Start building a backend against the controller at `controller_url`
    /// (e.g. `"https://127.0.0.1:8889"`). Defaults: 30s timeout, 1 MiB output cap,
    /// `EgressPolicy::deny_all()`.
    pub fn builder(controller_url: impl Into<String>) -> ForkdBackendBuilder {
        ForkdBackendBuilder {
            controller_url: controller_url.into(),
            bearer_token: None,
            controller_ca: None,
            snapshot: None,
            timeout: DEFAULT_TIMEOUT,
            max_output_bytes: DEFAULT_MAX_OUTPUT,
            egress: EgressPolicy::deny_all(),
        }
    }

    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &impl serde::Serialize,
    ) -> Result<T, ToolError> {
        // The bearer token rides only in the Authorization header — never in the
        // URL/body — so reqwest's error Display (URL only) cannot leak it.
        let resp = self
            .client
            .post(url)
            .bearer_auth(&self.token)
            .json(body)
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

    async fn fork(&self) -> Result<String, ToolError> {
        let url = format!("{}/v1/sandboxes", self.base);
        let body = ForkReq {
            snapshot_tag: &self.snapshot,
            n: 1,
            per_child_netns: true,
        };
        // Fork returns an array (n children); we requested 1, so take the first.
        let list: Vec<SandboxInfo> =
            tokio::time::timeout(self.timeout, self.post_json(&url, &body))
                .await
                .map_err(|_| ToolError::Other(anyhow::anyhow!("forkd: fork timed out")))??;
        list.into_iter()
            .next()
            .map(|s| s.id)
            .ok_or_else(|| ToolError::Other(anyhow::anyhow!("forkd returned no sandbox")))
    }

    async fn exec(&self, id: &str, command: &str) -> Result<ExecResp, ToolError> {
        let url = format!("{}/v1/sandboxes/{id}/exec", self.base);
        // `args` runs verbatim in the guest, so wrap the shell command.
        self.post_json(
            &url,
            &ExecReq {
                args: ["sh", "-c", command],
                timeout_secs: self.timeout.as_secs(),
            },
        )
        .await
    }

    async fn destroy(&self, id: &str) {
        // Best-effort teardown; failures here are not surfaced to the model.
        let url = format!("{}/v1/sandboxes/{id}", self.base);
        let _ = self.client.delete(&url).bearer_auth(&self.token).send().await;
    }
}

/// Truncate `s` to `cap` bytes on a char boundary; returns `(s, truncated)`.
fn truncate(mut s: String, cap: usize) -> (String, bool) {
    if s.len() <= cap {
        return (s, false);
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    (s, true)
}

#[async_trait]
impl ExecutionBackend for ForkdBackend {
    async fn run(&self, req: ExecRequest) -> Result<ExecOutput, ToolError> {
        let id = self.fork().await?;
        // The wall-clock command timeout governs exec; teardown always runs.
        let exec_result = tokio::time::timeout(self.timeout, self.exec(&id, &req.command)).await;
        let _ = tokio::time::timeout(CONTROL_TIMEOUT, self.destroy(&id)).await;
        match exec_result {
            Ok(Ok(resp)) => {
                let (stdout, t1) = truncate(resp.stdout, self.max_output_bytes);
                let (stderr, t2) = truncate(resp.stderr, self.max_output_bytes);
                Ok(ExecOutput::new(stdout, stderr, resp.exit_code, false, t1 || t2))
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Ok(ExecOutput::new(String::new(), String::new(), None, true, false)),
        }
    }

    fn guarantees(&self) -> SandboxGuarantees {
        SandboxGuarantees::new(
            Isolation::Virtualized, // filesystem — separate guest kernel + rootfs
            Isolation::None,        // network — egress NOT filtered yet (SMA-437)
            Isolation::Virtualized, // syscalls — guest kernel, not a host filter
            "forkd (firecracker microvm — experimental)",
        )
    }
}
```

In `crates/paigasus-helikon-tools/src/exec/mod.rs`, extend the microvm re-export:

```rust
#[cfg(feature = "microvm")]
pub use forkd::{EgressPolicy, ForkdBackend, ForkdBackendBuilder, ForkdError};
```

In `crates/paigasus-helikon-tools/src/lib.rs`, extend:

```rust
#[cfg(feature = "microvm")]
pub use exec::{EgressPolicy, ForkdBackend, ForkdBackendBuilder, ForkdError};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p paigasus-helikon-tools --features microvm`
Expected: PASS — the lib unit tests (incl. `guarantees_are_honest`, `builder_carries_egress_policy_and_requires_fields`) and `forks_execs_and_destroys`.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-tools/src/exec/forkd.rs crates/paigasus-helikon-tools/src/exec/mod.rs crates/paigasus-helikon-tools/src/lib.rs crates/paigasus-helikon-tools/tests/forkd_backend.rs
git commit -m "feat(tools): SMA-416 add ForkdBackend REST fork/exec/destroy skeleton"
```

---

## Task 6: Error mapping, timeout→teardown, token hygiene

**Files:**
- Modify: `crates/paigasus-helikon-tools/tests/forkd_backend.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/paigasus-helikon-tools/tests/forkd_backend.rs`:

```rust
use std::time::Duration;

#[tokio::test]
async fn controller_5xx_maps_to_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/sandboxes"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("test-token")
        .snapshot("snap-1")
        .build()
        .unwrap();
    let err = backend.run(ExecRequest::new("echo hi")).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("HTTP 500"), "unexpected error: {msg}");
    // Token hygiene: auth material never appears in the error text.
    assert!(!msg.contains("test-token"), "token leaked: {msg}");
}

#[tokio::test]
async fn exec_timeout_reports_timed_out_and_still_destroys() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/sandboxes"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{"id":"sb-2"}])),
        )
        .mount(&server)
        .await;
    // Exec hangs well past the command timeout.
    Mock::given(method("POST"))
        .and(path("/v1/sandboxes/sb-2/exec"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(30))
                .set_body_json(serde_json::json!({"stdout":"","stderr":"","exit_code":0})),
        )
        .mount(&server)
        .await;
    // Scoped destroy mock asserts teardown fired exactly once on the timeout path.
    let destroy = Mock::given(method("DELETE"))
        .and(path("/v1/sandboxes/sb-2"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount_as_scoped(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("test-token")
        .snapshot("snap-1")
        .timeout(Duration::from_millis(100))
        .build()
        .unwrap();
    let out = backend.run(ExecRequest::new("sleep 30")).await.unwrap();
    assert!(out.timed_out);
    assert_eq!(out.exit_code, None);
    // Dropping the scoped mock verifies the .expect(1) on destroy.
    drop(destroy);
}

#[tokio::test]
async fn output_over_cap_is_truncated() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/sandboxes"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{"id":"sb-3"}])),
        )
        .mount(&server)
        .await;
    let big = "x".repeat(50);
    Mock::given(method("POST"))
        .and(path("/v1/sandboxes/sb-3/exec"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"stdout": big, "stderr":"", "exit_code":0}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/sandboxes/sb-3"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let backend = ForkdBackend::builder(server.uri())
        .bearer_token("t")
        .snapshot("s")
        .max_output_bytes(10)
        .build()
        .unwrap();
    let out = backend.run(ExecRequest::new("yes")).await.unwrap();
    assert_eq!(out.stdout.len(), 10);
    assert!(out.truncated);
}
```

- [ ] **Step 2: Run tests to verify behavior**

Run: `cargo test -p paigasus-helikon-tools --features microvm --test forkd_backend`
Expected: PASS — all four integration tests. (The implementation from Task 5 already satisfies these; if any fail, fix the Task-5 code, not the tests.)

- [ ] **Step 3: Add the `#[ignore]`'d live-daemon test**

Append the live integration test (never silently green):

```rust
#[tokio::test]
#[ignore = "needs a live forkd controller + /dev/kvm; run on a Linux KVM host (SMA-437)"]
async fn live_forkd_runs_bash_in_a_microvm() {
    // Set FORKD_URL / FORKD_TOKEN / FORKD_SNAPSHOT to point at a real controller.
    let url = std::env::var("FORKD_URL").expect("FORKD_URL");
    let token = std::env::var("FORKD_TOKEN").expect("FORKD_TOKEN");
    let snapshot = std::env::var("FORKD_SNAPSHOT").expect("FORKD_SNAPSHOT");
    let backend = ForkdBackend::builder(url)
        .bearer_token(token)
        .snapshot(snapshot)
        .build()
        .unwrap();
    let out = backend
        .run(ExecRequest::new("echo from-a-microvm"))
        .await
        .unwrap();
    assert_eq!(out.stdout.trim(), "from-a-microvm");
    assert_eq!(out.exit_code, Some(0));
}
```

- [ ] **Step 4: Verify the ignored test is registered (not silently dropped)**

Run: `cargo test -p paigasus-helikon-tools --features microvm --test forkd_backend -- --list | grep live_forkd`
Expected: lists `live_forkd_runs_bash_in_a_microvm: test` (compiled, ignored).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-tools/tests/forkd_backend.rs
git commit -m "test(tools): SMA-416 cover forkd error/timeout/teardown + ignored live test"
```

---

## Task 7: Docs — mdBook + READMEs

**Files:**
- Modify: `docs/book/src/concepts/tools.md`
- Modify: `README.md`
- Modify: `crates/paigasus-helikon/README.md`
- Modify: `crates/paigasus-helikon-tools/README.md`

- [ ] **Step 1: mdBook microVM tier**

In `docs/book/src/concepts/tools.md`, find the section that describes the execution backends / containment ladder (`grep -n "OsSandboxBackend\|os-sandbox\|containment" docs/book/src/concepts/tools.md`). Add a `ForkdBackend` / microVM entry **above** `HostBackend` in the ladder (strongest tier), mirroring the `OsSandboxBackend` entry's style, with this honest framing:

> **`ForkdBackend` (microVM, `microvm` feature — experimental).** The strongest containment tier: each command runs in a KVM-isolated Firecracker microVM via the forkd controller (a REST client; the daemon needs Linux + `/dev/kvm`). **Skeleton status (SMA-416):** the fork→exec→destroy flow is implemented and mock-tested, but a live KVM run and egress *enforcement* are deferred to SMA-437. **Caveat:** the skeleton is **not network-contained today** — `guarantees().network` is honestly `None`, i.e. *weaker than `OsSandboxBackend`* on the egress axis until the layered netns + proxy policy lands. `Virtualized` on the other axes means "behind a VM boundary," not a syscall/path allowlist.

- [ ] **Step 2: README feature maps**

In each of the three READMEs, find the feature table/list that mentions `tools-os-sandbox` / `os-sandbox` (`grep -n "os-sandbox" <file>`) and add a sibling row for the microVM feature, mirroring the wording:
- `crates/paigasus-helikon-tools/README.md`: add `microvm` to the feature list — *"microVM Bash containment via the forkd Firecracker controller (REST client; experimental skeleton — SMA-416)."*
- `crates/paigasus-helikon/README.md` and `README.md`: add `tools-microvm` to the facade feature → module map, mirroring `tools-os-sandbox`.

Keep install snippets drift-free (`cargo add` form; no hardcoded versions).

- [ ] **Step 3: Verify the book builds clean**

Run: `mdbook build docs/book`
Expected: builds with no link-check errors (`warning-policy = "error"`).

- [ ] **Step 4: Commit**

```bash
git add docs/book/src/concepts/tools.md README.md crates/paigasus-helikon/README.md crates/paigasus-helikon-tools/README.md
git commit -m "docs(tools): SMA-416 document the microVM (forkd) containment tier"
```

---

## Task 8: Final CI gate sweep

**Files:** none (verification + fixups only)

- [ ] **Step 1: Format**

Run: `cargo fmt --all -- --check`
Expected: clean. If not, run `cargo fmt --all` and re-check.

- [ ] **Step 2: Clippy (all features, all targets)**

Run: `cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: clean. Fix any warnings in `forkd.rs` (common: needless borrows, `format!` in args).

- [ ] **Step 3: Tests (all features)**

Run: `cargo test --workspace --all-features`
Expected: PASS (the ignored live test stays ignored).

- [ ] **Step 4: Docs build (warnings = errors)**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
Expected: clean — every new `pub` item is documented (no `missing_docs`, no broken intra-doc links). Note: do NOT link a `pub` item's `///` to a private item (e.g. don't `[`EgressPolicy`]`-link from a pub doc to a private field).

- [ ] **Step 5: Doc coverage**

Run:
```bash
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
```
Expected: ≥ 80%.

- [ ] **Step 6: Default build unaffected**

Run: `cargo build -p paigasus-helikon-tools`
Expected: builds WITHOUT reqwest (the `microvm` path is feature-gated off by default).

- [ ] **Step 7: Commit any fixups**

```bash
git add -A
git commit -m "chore(tools): SMA-416 satisfy fmt/clippy/doc gates for forkd backend"
```
(Skip if Steps 1–6 needed no changes.)

---

## Self-review notes (for the executor)

- **Spec coverage:** Task 1 = spike note (AC 1). Tasks 2–6 = `ForkdBackend` skeleton + `Isolation::Virtualized` + honest `guarantees()` + `EgressPolicy` carried + `#[ignore]`'d live test (AC 2, re-scoped). Task 7 = docs. Task 8 = CI gates. The E2B sibling is verified-not-built (the trait is unchanged + object-safe). The portable-client (no target gate) decision is realized in Task 3 (`microvm = ["dep:reqwest"]`, no `cfg(target_os)`).
- **Type consistency:** `into_backend()` (private, used by `build()` and unit tests) returns the concrete `ForkdBackend`; `build()` returns `Arc<dyn ExecutionBackend>`. `EgressPolicy::is_allowed`, `ForkdError` variants, and the REST structs (`ForkReq`/`SandboxInfo`/`ExecReq`/`ExecResp`) are referenced consistently across tasks.
- **Egress honesty:** `guarantees().network == Isolation::None` is asserted in `guarantees_are_honest` — the skeleton never claims egress containment it doesn't enforce.
- **No manual version bumps** — release-plz owns them.

## Execution handoff

PR title (squashed `main` commit — must be Conventional-Commit + lowercase subject after the `SMA-###`):
`feat(tools): SMA-416 add forkd microVM ExecutionBackend skeleton + spike note`
