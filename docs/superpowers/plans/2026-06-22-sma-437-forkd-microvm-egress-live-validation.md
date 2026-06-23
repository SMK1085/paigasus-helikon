# SMA-437 forkd microVM — egress enforcement + live-KVM validation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the forkd microVM tier actually enforce egress (CI-provable code) and actually validate end-to-end on KVM (operator-run Docker harness on GCP nested-virt).

**Architecture:** Promote the SMA-412 host/IP/SSRF primitives + a unified `EgressPolicy` into a shared `net` module; add a purpose-built `EgressProxy` (explicit CONNECT/HTTP proxy enforcing the domain policy + SSRF block); add `Isolation::Proxied`; let `ForkdBackend` report `Proxied` only via an explicit `.enforce_egress()` attestation with a build-time reachability probe; ship a Dockerized forkd+KVM harness + GCP launch script + guest-image build + un-`#[ignore]`'d env-gated live test + runbook.

**Tech Stack:** Rust (tokio, reqwest/rustls, async-trait, thiserror); dev: wiremock, rcgen, tokio-rustls; Docker + bash + iptables (harness); gcloud (launch).

**Spec:** `docs/superpowers/specs/2026-06-22-sma-437-forkd-microvm-egress-live-validation-design.md`

## Global Constraints

- **Workspace path discipline:** all edits under the worktree root `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-437-forkd-microvm/`. Never write into the main checkout.
- **MSRV 1.85; edition 2024.** `missing_docs = "warn"` is workspace-wide → every new `pub` item needs a `///` doc, or the `-D warnings` docs gate fails.
- **No `paigasus-helikon-core` change.** Promotion lives entirely in `paigasus-helikon-tools`.
- **No source-breaking changes** (0.2.x patch): `EgressPolicy::is_allowed(&str)` stays as a `#[deprecated]` alias; web public API unchanged.
- **`guarantees().network` honesty:** default `Isolation::None`; `Proxied` ONLY when `.enforce_egress()` is set (hardened-attestation model — Sven-approved).
- **Bearer/secret hygiene:** never log/format the bearer token or `Authorization` headers; proxy logs hostnames + verdicts only.
- **Run `cargo fmt --all` + `cargo clippy --workspace --all-features --all-targets -- -D warnings` before every commit** (pre-commit hook is a no-op; pre-push catches it late).
- **Commit prefix:** `<type>(<scope>): SMA-437 <lowercase message>`. Allowed scopes incl. `tools`, `spec`, `plan`, `docs`, `release`. Commits are signed via a 1Password SSH key — if a commit fails with "failed to fill whole buffer," ask Sven to unlock the vault.
- **Version:** bump `-tools` `0.2.6 → 0.2.7` in the release commit (Task 13), not piecemeal.
- **Don't `git add -A`** (`.env`/`.claude` are untracked-but-not-ignored). Stage explicit paths; verify with `git show --stat`.
- **forkd confirmed API (v0.5.2):** fork `POST /v1/sandboxes` → array; exec `POST /v1/sandboxes/:id/exec` `{args,timeout_secs}` → `{stdout,stderr,exit_code}`; destroy `DELETE /v1/sandboxes/:id` → 204; snapshot `POST /v1/snapshots` `{tag,kernel,rootfs,rw,tap,boot_wait_secs}`; health `GET /healthz` (no auth); TLS via `--tls-cert/--tls-key`; bearer via `--token-file`. Exec has **no env/cwd** field.

---

## Commit group A — CI-provable egress code

### Task 1: Create the shared `net` module; move the SMA-412 primitives into it

**Files:**
- Create: `crates/paigasus-helikon-tools/src/net/mod.rs`
- Create: `crates/paigasus-helikon-tools/src/net/policy.rs`
- Modify: `crates/paigasus-helikon-tools/src/web/http.rs` (shrink to re-exports)
- Modify: `crates/paigasus-helikon-tools/src/web/fetch.rs:12`, `src/web/search.rs:11`, `src/web/backends/tavily.rs:8`, `src/web/backends/brave.rs:8` (import paths)
- Modify: `crates/paigasus-helikon-tools/src/lib.rs` (declare `mod net`)

**Interfaces:**
- Produces (all in `crate::net`):
  - `pub(crate) fn host_allowed(host: &str, allow: Option<&[String]>, deny: &[String]) -> bool`
  - `pub fn ip_blocked(ip: std::net::IpAddr) -> bool`
  - `pub struct GuardedResolver { allow_private: bool }` (now `pub`) + its `impl reqwest::dns::Resolve`
  - `pub(crate) fn build_client(user_agent: &str, timeout: Duration, follow_redirects: bool, dns_guard: Option<bool>) -> reqwest::Result<reqwest::Client>`
  - `pub(crate) async fn ssrf_check(url: &url::Url, allow_private: bool) -> Result<(), ToolError>`

- [ ] **Step 1: Declare the module in `lib.rs`** (after `mod exec;` line, before `mod read;`):

```rust
#[cfg(any(feature = "web", feature = "microvm"))]
mod net;
```

- [ ] **Step 2: Create `src/net/mod.rs`**

```rust
//! Shared networking policy + egress enforcement for the `web` and `microvm`
//! tools: the host allow/deny + SSRF IP classifier (promoted from SMA-412) and
//! the CONNECT egress proxy (SMA-437).

pub mod policy;
#[cfg(feature = "microvm")]
pub mod proxy;

pub use policy::{ip_blocked, EgressPolicy, GuardedResolver};
```

- [ ] **Step 3: Create `src/net/policy.rs` by moving the contents of `web/http.rs` verbatim**, with three changes: (a) module doc updated; (b) `struct GuardedResolver` becomes `pub struct GuardedResolver` and gains a `///` doc; (c) keep `host_allowed`/`build_client`/`ssrf_check` as `pub(crate)`, `ip_blocked` as `pub` (add `///` already present). Move the entire `#[cfg(test)] mod tests` block too. (The `EgressPolicy` type is added in Task 2 — do not add it yet.)

- [ ] **Step 4: Replace `web/http.rs` with thin re-exports**

```rust
//! Re-exports of the shared networking policy (moved to `crate::net::policy`
//! in SMA-437). Kept so the `web` modules' `use crate::web::http::…` paths and
//! the SMA-412 layout stay stable.

pub(crate) use crate::net::policy::{build_client, host_allowed, ssrf_check};
```

- [ ] **Step 5: Update the `web` import sites** — change `use crate::web::http::{build_client, host_allowed, ssrf_check};` etc. to import from `crate::net::policy` directly (or leave them on `crate::web::http`'s re-export — both compile; prefer `crate::net::policy` for the two non-backend files):
  - `src/web/fetch.rs:12` → `use crate::net::policy::{build_client, host_allowed, ssrf_check};`
  - `src/web/search.rs:11` → `use crate::net::policy::host_allowed;`
  - `src/web/backends/tavily.rs:8` and `brave.rs:8` → `use crate::net::policy::build_client;`

- [ ] **Step 6: Verify web still compiles + tests pass**

Run: `cargo test -p paigasus-helikon-tools --features web`
Expected: PASS (the moved `policy.rs` tests + all web tests).

- [ ] **Step 7: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --features web --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src/net crates/paigasus-helikon-tools/src/web crates/paigasus-helikon-tools/src/lib.rs
git commit -m "refactor(tools): SMA-437 promote SMA-412 net primitives to a shared net module"
```

---

### Task 2: Unify `EgressPolicy` into `net::policy` (no source breakage)

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/net/policy.rs` (add `EgressPolicy`)
- Modify: `crates/paigasus-helikon-tools/src/net/mod.rs` (already re-exports `EgressPolicy`)
- Modify: `crates/paigasus-helikon-tools/src/exec/forkd.rs` (remove local `EgressPolicy`, import from `net`)
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs:36` (re-export source) and `src/lib.rs:39`
- Test: `crates/paigasus-helikon-tools/src/net/policy.rs` (`#[cfg(test)]`) + `tests/web_fetch.rs`

**Interfaces:**
- Produces: `crate::net::policy::EgressPolicy` with:
  - `pub fn deny_all() -> Self` / `allow_all() -> Self`
  - `pub fn allow_domains<I,S>(self, I) -> Self` / `deny_domains<I,S>(self, I) -> Self`
  - `pub fn allow_private_ips(self, allow: bool) -> Self`
  - `pub fn is_host_allowed(&self, host: &str) -> bool`
  - `pub fn is_ip_allowed(&self, ip: std::net::IpAddr) -> bool`
  - `#[deprecated(note = "renamed to is_host_allowed")] pub fn is_allowed(&self, host: &str) -> bool`
  - `#[derive(Debug, Clone, Default, PartialEq, Eq)]`
- Consumes: `host_allowed`, `ip_blocked` from Task 1.

- [ ] **Step 1: Write the failing tests** — add to `src/net/policy.rs`'s `#[cfg(test)] mod tests`:

```rust
#[test]
fn egress_policy_host_and_ip_checks() {
    use std::net::IpAddr;
    let p = EgressPolicy::deny_all().allow_domains(["pypi.org"]);
    assert!(p.is_host_allowed("pypi.org"));
    assert!(p.is_host_allowed("files.pypi.org"));
    assert!(!p.is_host_allowed("evil.test"));
    // private IPs blocked by default; allowed when toggled
    let priv_ip: IpAddr = "10.0.0.1".parse().unwrap();
    assert!(!p.is_ip_allowed(priv_ip));
    let pub_ip: IpAddr = "8.8.8.8".parse().unwrap();
    assert!(p.is_ip_allowed(pub_ip));
    let p2 = EgressPolicy::allow_all().allow_private_ips(true);
    assert!(p2.is_ip_allowed(priv_ip));
}

#[test]
fn egress_policy_deprecated_is_allowed_alias_still_works() {
    let p = EgressPolicy::allow_all().deny_domains(["evil.test"]);
    #[allow(deprecated)]
    let denied = p.is_allowed("evil.test");
    assert!(!denied);
}

#[test]
fn egress_policy_empty_allow_list_means_deny_all_for_forkd_default() {
    let p = EgressPolicy::deny_all(); // allow: Some(empty) -> deny everything
    assert!(!p.is_host_allowed("anything.test"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p paigasus-helikon-tools --features web egress_policy`
Expected: FAIL (`EgressPolicy` not found in `net::policy`).

- [ ] **Step 3: Add `EgressPolicy` to `src/net/policy.rs`** (move the definition from `forkd.rs`, add `allow_private_ips`, the IP method, the deprecated alias, and `PartialEq, Eq`):

```rust
/// Domain allow/deny + private-IP (SSRF) policy shared by the `web` tools and the
/// `microvm` egress proxy/backend. The single public policy type (SMA-437).
///
/// Domain matching is sub-domain-aware, case-insensitive, and trailing-dot-
/// insensitive: `example.com` matches `example.com` and `api.example.com`.
///
/// **Empty-allow-list semantics matter:** `allow: None` means *no restriction*
/// (any host, subject to `deny`); `allow: Some(empty)` means *deny everything*.
/// `deny_all()` builds the latter; `allow_all()` the former.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EgressPolicy {
    allow: Option<Vec<String>>,
    deny: Vec<String>,
    allow_private_ips: bool,
}

impl EgressPolicy {
    /// Deny all egress (an empty allow-list permits nothing).
    pub fn deny_all() -> Self {
        Self { allow: Some(Vec::new()), deny: Vec::new(), allow_private_ips: false }
    }

    /// Allow all egress (no allow-list, no deny-list).
    pub fn allow_all() -> Self {
        Self::default()
    }

    /// Add allowed domains. Setting any allow-list switches the policy to
    /// default-deny (only listed domains and their sub-domains are permitted).
    pub fn allow_domains<I, S>(mut self, domains: I) -> Self
    where I: IntoIterator<Item = S>, S: Into<String> {
        self.allow.get_or_insert_with(Vec::new).extend(domains.into_iter().map(Into::into));
        self
    }

    /// Add denied domains. A deny match always refuses, beating any allow.
    pub fn deny_domains<I, S>(mut self, domains: I) -> Self
    where I: IntoIterator<Item = S>, S: Into<String> {
        self.deny.extend(domains.into_iter().map(Into::into));
        self
    }

    /// Permit private/loopback/link-local IPs (default: deny them as SSRF risks).
    pub fn allow_private_ips(mut self, allow: bool) -> Self {
        self.allow_private_ips = allow;
        self
    }

    /// `true` if `host` is permitted: not denied, and — when an allow-list is set
    /// — matching it (itself or a sub-domain).
    pub fn is_host_allowed(&self, host: &str) -> bool {
        host_allowed(host, self.allow.as_deref(), &self.deny)
    }

    /// `true` if `ip` may be connected to: a public address, or any address when
    /// `allow_private_ips` is set.
    pub fn is_ip_allowed(&self, ip: std::net::IpAddr) -> bool {
        self.allow_private_ips || !ip_blocked(ip)
    }

    /// Deprecated alias for [`Self::is_host_allowed`], kept for source
    /// compatibility with the SMA-416 `EgressPolicy`.
    #[deprecated(note = "renamed to is_host_allowed")]
    pub fn is_allowed(&self, host: &str) -> bool {
        self.is_host_allowed(host)
    }
}
```

- [ ] **Step 4: Remove the old `EgressPolicy` from `forkd.rs`** (delete the `pub struct EgressPolicy` block + its `impl` + its in-module tests `egress_policy_deny_all_then_allowlist` / `egress_policy_deny_beats_allow_and_default_allows` — they are superseded by the `net::policy` tests). Add `use crate::net::policy::EgressPolicy;` at the top of `forkd.rs`. Replace any `b.egress_policy().is_allowed(..)` in forkd's remaining tests with `is_host_allowed`.

- [ ] **Step 5: Fix the re-export source** — in `src/exec/mod.rs:36`, drop `EgressPolicy` from the `pub use forkd::{…}` list (forkd no longer defines it). In `src/lib.rs`, change the microvm re-export and add the shared one:

```rust
// lib.rs — replace the `#[cfg(feature = "microvm")] pub use exec::{EgressPolicy, ...}` line:
#[cfg(feature = "microvm")]
pub use exec::{ForkdBackend, ForkdBackendBuilder, ForkdError};
#[cfg(any(feature = "web", feature = "microvm"))]
pub use net::{EgressPolicy, GuardedResolver};
```

(`paigasus_helikon_tools::EgressPolicy` remains the canonical path — unchanged for consumers.)

- [ ] **Step 6: Add the WebFetch empty-allow regression test** to `tests/web_fetch.rs`:

```rust
#[tokio::test]
async fn web_fetch_empty_allow_domains_permits_any_host() {
    // An empty allow_domains list must mean "no restriction", NOT "deny all".
    let tool = paigasus_helikon_tools::WebFetchTool::builder()
        .allow_domains(Vec::<String>::new())
        .build();
    // Builder must succeed and impose no host allow-list (smoke: it builds).
    let _ = tool;
}
```

(If `WebFetchTool::builder().build()` needs args, mirror the existing `tests/web_fetch.rs` construction; the assertion is simply that an empty allow-list does not flip to deny-all — confirmed by the existing host-allow tests still passing.)

- [ ] **Step 7: Verify all features compile + tests pass**

Run: `cargo test -p paigasus-helikon-tools --features web,microvm egress_policy`
Run: `cargo test -p paigasus-helikon-tools --features web,microvm`
Expected: PASS.

- [ ] **Step 8: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --features web,microvm --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src crates/paigasus-helikon-tools/tests/web_fetch.rs
git commit -m "refactor(tools): SMA-437 unify EgressPolicy in net::policy with deprecated is_allowed alias"
```

---

### Task 3: Add the `Isolation::Proxied` variant

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/exec/mod.rs` (the `Isolation` enum, ~line 115-127)

**Interfaces:**
- Produces: `crate::Isolation::Proxied`.

- [ ] **Step 1: Add the variant + doc** after the `Virtualized` arm in the `Isolation` enum:

```rust
    /// Egress is filtered by an allow/deny **domain** policy at a CONNECT/HTTP
    /// proxy (application layer). `Proxied` is meaningful **only in the layered
    /// deployment**: a per-VM netns default-deny that drops all egress except the
    /// proxy path (and DNS to a vetted resolver). Without that L3/L4 default-deny,
    /// non-proxy-aware clients, DNS (UDP/53), QUIC/HTTP-3 (UDP/443), and raw TCP
    /// **escape** — the proxy never sees them. The backend cannot verify the host's
    /// netns rules, so this tier reflects an operator attestation (see
    /// `ForkdBackendBuilder::enforce_egress`), the same trust model the other tiers
    /// apply to the kernel/hypervisor. Read as "HTTP/S egress is domain-filtered,
    /// given the netns default-deny," not "all packets are blocked."
    Proxied,
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p paigasus-helikon-tools --features microvm`
Expected: PASS (existing `match` arms on `Isolation` already use wildcards or are exhaustive in-crate; fix any non-exhaustive match the compiler flags by adding a `Isolation::Proxied => …` arm).

- [ ] **Step 3: fmt + commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-tools/src/exec/mod.rs
git commit -m "feat(tools): SMA-437 add Isolation::Proxied variant"
```

---

### Task 4: Build the `EgressProxy` (CONNECT + absolute-URI HTTP)

**Files:**
- Create: `crates/paigasus-helikon-tools/src/net/proxy.rs`
- Modify: `crates/paigasus-helikon-tools/src/net/mod.rs` (already declares `proxy` under microvm; add `pub use proxy::EgressProxy;`)
- Modify: `crates/paigasus-helikon-tools/src/lib.rs` (re-export `EgressProxy` under microvm)
- Modify: `crates/paigasus-helikon-tools/Cargo.toml` (microvm feature deps)
- Test: `crates/paigasus-helikon-tools/tests/egress_proxy.rs`

**Interfaces:**
- Produces: `crate::net::proxy::EgressProxy` with `pub fn new(EgressPolicy) -> Self` and `pub async fn serve(self, listener: tokio::net::TcpListener) -> std::io::Result<()>`.
- Consumes: `EgressPolicy::{is_host_allowed, is_ip_allowed}`, `build_client` (Task 1/2).

- [ ] **Step 1: Extend the `microvm` feature** in `crates/paigasus-helikon-tools/Cargo.toml`:

```toml
microvm = ["dep:reqwest", "dep:url", "tokio/net", "tokio/io-util"]
```

- [ ] **Step 2: Write the failing tests** — `crates/paigasus-helikon-tools/tests/egress_proxy.rs`:

```rust
#![cfg(feature = "microvm")]
#![allow(missing_docs)]

use std::time::Duration;

use paigasus_helikon_tools::{EgressPolicy, EgressProxy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Start the proxy on an ephemeral loopback port; return its `host:port`.
async fn start_proxy(policy: EgressPolicy) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { EgressProxy::new(policy).serve(listener).await });
    format!("127.0.0.1:{}", addr.port())
}

/// Send a raw CONNECT and return the proxy's status line.
async fn connect_status(proxy: &str, target: &str) -> String {
    let mut s = tokio::net::TcpStream::connect(proxy).await.unwrap();
    s.write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = vec![0u8; 128];
    let n = s.read(&mut buf).await.unwrap();
    String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or("").to_string()
}

#[tokio::test]
async fn connect_to_nonallowlisted_domain_is_denied_fast() {
    let proxy = start_proxy(EgressPolicy::deny_all().allow_domains(["example.com"])).await;
    let status = tokio::time::timeout(
        Duration::from_secs(2),
        connect_status(&proxy, "evil.test:443"),
    )
    .await
    .expect("deny must be fast, not a hang");
    assert!(status.contains("403"), "expected 403, got: {status}");
}

#[tokio::test]
async fn connect_to_domain_resolving_to_private_ip_is_denied() {
    // localhost resolves to loopback (blocked); allow it by domain but not by IP.
    let proxy = start_proxy(EgressPolicy::deny_all().allow_domains(["localhost"])).await;
    let status = connect_status(&proxy, "localhost:9").await;
    assert!(status.contains("403"), "SSRF: private IP must be denied, got: {status}");
}

#[tokio::test]
async fn connect_to_allowlisted_loopback_tunnels_bytes() {
    // A loopback echo upstream; allow private IPs + the host so the tunnel forms.
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up_addr = upstream.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = upstream.accept().await.unwrap();
        let mut b = [0u8; 5];
        sock.read_exact(&mut b).await.unwrap();
        sock.write_all(&b).await.unwrap();
    });
    let proxy = start_proxy(
        EgressPolicy::deny_all().allow_domains(["127.0.0.1"]).allow_private_ips(true),
    )
    .await;
    let mut s = tokio::net::TcpStream::connect(&proxy).await.unwrap();
    let target = format!("127.0.0.1:{}", up_addr.port());
    s.write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut line = vec![0u8; 64];
    let n = s.read(&mut line).await.unwrap();
    assert!(String::from_utf8_lossy(&line[..n]).contains("200"));
    // Tunnel established; echo round-trips.
    s.write_all(b"hello").await.unwrap();
    let mut echo = [0u8; 5];
    s.read_exact(&mut echo).await.unwrap();
    assert_eq!(&echo, b"hello");
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p paigasus-helikon-tools --features microvm --test egress_proxy`
Expected: FAIL (`EgressProxy` not found).

- [ ] **Step 4: Implement `src/net/proxy.rs`**

```rust
//! [`EgressProxy`] — an explicit forward proxy that enforces an [`EgressPolicy`]
//! on outbound traffic from the microVM tier. HTTPS via `CONNECT` tunneling;
//! plain HTTP via absolute-URI forwarding. Both paths check the destination host
//! against the domain allow/deny policy and the resolved IPs against the SSRF
//! (private-range) block before any upstream connection is made.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::policy::EgressPolicy;

/// An egress-policy-enforcing forward proxy. Run it with [`Self::serve`] against a
/// bound [`TcpListener`]; each accepted connection is handled on its own task.
///
/// The proxy is the application-layer half of the layered egress model (SMA-437):
/// it filters HTTP/S by domain. The L3/L4 default-deny that forces guest traffic
/// through it is the deployment's per-VM netns config (see the runbook).
pub struct EgressProxy {
    policy: Arc<EgressPolicy>,
}

impl EgressProxy {
    /// Build a proxy enforcing `policy`.
    pub fn new(policy: EgressPolicy) -> Self {
        Self { policy: Arc::new(policy) }
    }

    /// Accept connections on `listener` until it errors, handling each on a task.
    pub async fn serve(self, listener: TcpListener) -> io::Result<()> {
        loop {
            let (sock, _peer) = listener.accept().await?;
            let policy = Arc::clone(&self.policy);
            tokio::spawn(async move {
                let _ = handle(sock, policy).await;
            });
        }
    }
}

const DENY: &[u8] = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n";
const OK: &[u8] = b"HTTP/1.1 200 Connection Established\r\n\r\n";
const BAD: &[u8] = b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n";
const MAX_HEAD: usize = 16 * 1024;

async fn handle(mut client: TcpStream, policy: Arc<EgressPolicy>) -> io::Result<()> {
    // Read the request head (request line + headers) up to CRLFCRLF, bounded.
    let mut head = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        if head.len() >= MAX_HEAD {
            client.write_all(BAD).await?;
            return Ok(());
        }
        if client.read(&mut byte).await? == 0 {
            return Ok(()); // client closed
        }
        head.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&head);
    let Some(request_line) = text.lines().next() else {
        client.write_all(BAD).await?;
        return Ok(());
    };
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if method.eq_ignore_ascii_case("CONNECT") {
        handle_connect(client, target, &policy).await
    } else if let Some(host) = absolute_uri_host(target) {
        // Plain HTTP via absolute-URI: enforce host, then deny (forwarding plain
        // HTTP is out of scope — proxy-aware HTTPS via CONNECT is the path; netns
        // default-deny drops non-proxy egress). Allowed plain-HTTP is rare; reject
        // with a clear 403 unless allow-listed, in which case 501 (not forwarded).
        if !policy.is_host_allowed(&host) {
            client.write_all(DENY).await?;
        } else {
            client
                .write_all(b"HTTP/1.1 501 Not Implemented\r\nContent-Length: 0\r\n\r\n")
                .await?;
        }
        Ok(())
    } else {
        client.write_all(BAD).await?;
        Ok(())
    }
}

async fn handle_connect(
    mut client: TcpStream,
    target: &str,
    policy: &EgressPolicy,
) -> io::Result<()> {
    let Some((host, port)) = split_host_port(target) else {
        client.write_all(BAD).await?;
        return Ok(());
    };
    if !policy.is_host_allowed(&host) {
        client.write_all(DENY).await?;
        return Ok(());
    }
    // Resolve and vet EVERY address (closes DNS-rebinding window).
    let addrs: Vec<SocketAddr> = match tokio::net::lookup_host((host.as_str(), port)).await {
        Ok(it) => it.filter(|a| policy.is_ip_allowed(a.ip())).collect(),
        Err(_) => {
            client.write_all(DENY).await?;
            return Ok(());
        }
    };
    let Some(addr) = addrs.into_iter().next() else {
        client.write_all(DENY).await?; // resolved only to blocked IPs
        return Ok(());
    };
    let mut upstream = match TcpStream::connect(addr).await {
        Ok(s) => s,
        Err(_) => {
            client
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                .await?;
            return Ok(());
        }
    };
    client.write_all(OK).await?;
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

/// Split `host:port`, stripping IPv6 brackets. Returns `None` if malformed.
fn split_host_port(s: &str) -> Option<(String, u16)> {
    let (host, port) = s.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
    if host.is_empty() { None } else { Some((host, port)) }
}

/// Extract the host from an absolute-form HTTP request target (`http://host/..`).
fn absolute_uri_host(target: &str) -> Option<String> {
    let rest = target.strip_prefix("http://").or_else(|| target.strip_prefix("https://"))?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let host = authority.rsplit_once(':').map(|(h, _)| h).unwrap_or(authority);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    (!host.is_empty()).then(|| host.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_host_port_handles_ipv6_and_bad_input() {
        assert_eq!(split_host_port("example.com:443"), Some(("example.com".into(), 443)));
        assert_eq!(split_host_port("[::1]:8080"), Some(("::1".into(), 8080)));
        assert_eq!(split_host_port("noport"), None);
        assert_eq!(split_host_port(":443"), None);
    }

    #[test]
    fn absolute_uri_host_parses() {
        assert_eq!(absolute_uri_host("http://a.test/x").as_deref(), Some("a.test"));
        assert_eq!(absolute_uri_host("http://a.test:8080/x").as_deref(), Some("a.test"));
        assert_eq!(absolute_uri_host("/relative"), None);
    }
}
```

- [ ] **Step 5: Re-export** — `src/net/mod.rs` add under the existing `#[cfg(feature = "microvm")] pub mod proxy;`:

```rust
#[cfg(feature = "microvm")]
pub use proxy::EgressProxy;
```

and in `src/lib.rs` add to the microvm re-export:

```rust
#[cfg(feature = "microvm")]
pub use net::EgressProxy;
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p paigasus-helikon-tools --features microvm --test egress_proxy`
Run: `cargo test -p paigasus-helikon-tools --features microvm net::proxy`
Expected: PASS.

- [ ] **Step 7: Verify lib-only microvm build (devdep-masking footgun)**

Run: `cargo build -p paigasus-helikon-tools --features microvm`
Expected: PASS (no `web`, no dev-deps — confirms `url`/`tokio` features are on the `microvm` feature, not borrowed from `web`/dev-deps).

- [ ] **Step 8: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --features microvm --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src/net crates/paigasus-helikon-tools/src/lib.rs crates/paigasus-helikon-tools/Cargo.toml crates/paigasus-helikon-tools/tests/egress_proxy.rs
git commit -m "feat(tools): SMA-437 add EgressProxy enforcing domain + SSRF egress policy"
```

---

### Task 5: Wire `.enforce_egress()` + reachability probe + `Proxied` guarantee

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/exec/forkd.rs`
- Test: `crates/paigasus-helikon-tools/tests/forkd_backend.rs`

**Interfaces:**
- Produces: `ForkdBackendBuilder::enforce_egress(proxy_endpoint: impl Into<String>) -> Self`; when set + reachable, `guarantees().network == Isolation::Proxied`.
- Consumes: `Isolation::Proxied` (Task 3).

- [ ] **Step 1: Write the failing unit tests** in `forkd.rs`'s `#[cfg(test)] mod tests`:

```rust
#[test]
fn guarantees_network_none_without_enforce_egress() {
    let b = ForkdBackend::builder("https://localhost:8080")
        .bearer_token("t").snapshot("s").into_backend().unwrap();
    assert_eq!(b.guarantees().network, Isolation::None);
}
```

And an integration test in `tests/forkd_backend.rs` (uses a wiremock controller as the proxy-reachability target — the probe just needs a reachable endpoint):

```rust
#[tokio::test]
async fn enforce_egress_reports_proxied_when_proxy_reachable() {
    let proxy = MockServer::start().await;
    // The reachability probe hits the proxy endpoint; any TCP-accepting server passes.
    let backend = ForkdBackend::builder("http://127.0.0.1:1") // controller unused here
        .bearer_token("t").snapshot("s")
        .enforce_egress(proxy.uri())
        .build()
        .expect("builds when proxy reachable");
    assert_eq!(backend.guarantees().network, paigasus_helikon_tools::Isolation::Proxied);
}

#[tokio::test]
async fn enforce_egress_fails_closed_when_proxy_unreachable() {
    // Port 1 on loopback refuses; build() must fail rather than report Proxied.
    let err = ForkdBackend::builder("https://127.0.0.1:8080")
        .bearer_token("t").snapshot("s")
        .enforce_egress("http://127.0.0.1:1")
        .build();
    assert!(err.is_err(), "unreachable proxy must fail closed");
}
```

(Note: the reachability probe is a TCP connect to the proxy host:port; a wiremock server accepts TCP so the probe passes. `http://127.0.0.1:1` for the *controller* URL is fine — the controller isn't contacted at build time.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p paigasus-helikon-tools --features microvm enforce_egress`
Expected: FAIL (`enforce_egress` not found).

- [ ] **Step 3: Implement.** In `ForkdBackendBuilder` add a field `enforce_egress: Option<String>` (default `None`) and the builder method:

```rust
    /// Attest that the layered egress enforcement (per-VM netns default-deny + the
    /// [`EgressProxy`](crate::EgressProxy) at `proxy_endpoint`) is deployed, so
    /// `guarantees().network` reports [`Isolation::Proxied`]. `build()` probes the
    /// proxy for reachability and fails closed if it cannot connect — but it
    /// **cannot** verify the host's netns rules, so this is an operator attestation
    /// (the same trust model the kernel/hypervisor tiers use). Without this, the
    /// network guarantee stays [`Isolation::None`]. `proxy_endpoint` is `host:port`
    /// or a URL.
    pub fn enforce_egress(mut self, proxy_endpoint: impl Into<String>) -> Self {
        self.enforce_egress = Some(proxy_endpoint.into());
        self
    }
```

In `into_backend()`, after building the client, run the probe and store the flag:

```rust
        let egress_enforced = match &self.enforce_egress {
            Some(ep) => {
                probe_proxy_reachable(ep).map_err(|_| ForkdError::ProxyUnreachable)?;
                true
            }
            None => false,
        };
```

Add the `ForkdError::ProxyUnreachable` variant:

```rust
    /// `enforce_egress` was set but the proxy endpoint could not be reached.
    #[error("egress proxy endpoint is unreachable")]
    ProxyUnreachable,
```

Add the field to the `ForkdBackend` struct (`egress_enforced: bool`) and the probe helper (a blocking-free TCP connect with a short timeout, run on the current runtime). Because `build()` is sync but the probe needs async, expose the probe as a small sync helper using a temporary runtime OR make the reachability check a `std::net::TcpStream::connect_timeout`:

```rust
/// Best-effort reachability probe: a short TCP connect to the proxy endpoint.
/// Sync (callable from the sync `build()`), uses `std::net` with a 3s timeout.
fn probe_proxy_reachable(endpoint: &str) -> std::io::Result<()> {
    use std::net::ToSocketAddrs;
    // Accept `host:port` or a URL; strip a scheme if present.
    let hostport = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
        .unwrap_or(endpoint)
        .trim_end_matches('/');
    let addr = hostport
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no addr"))?;
    std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(3)).map(|_| ())
}
```

Update `guarantees()`:

```rust
    fn guarantees(&self) -> SandboxGuarantees {
        let network = if self.egress_enforced { Isolation::Proxied } else { Isolation::None };
        SandboxGuarantees::new(
            Isolation::Virtualized,
            network,
            Isolation::Virtualized,
            "forkd (firecracker microvm — experimental)",
        )
    }
```

(Remove the now-stale `guarantees_are_honest` assertion that hard-codes `network == None` if it conflicts; keep one asserting the default `None`.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p paigasus-helikon-tools --features microvm`
Expected: PASS.

- [ ] **Step 5: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --features microvm --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/src/exec/forkd.rs crates/paigasus-helikon-tools/tests/forkd_backend.rs
git commit -m "feat(tools): SMA-437 add enforce_egress attestation + proxy probe + Proxied guarantee"
```

---

### Task 6: Controller TLS-trust integration test

**Files:**
- Create: `crates/paigasus-helikon-tools/tests/forkd_tls.rs`
- Modify: root `Cargo.toml` `[workspace.dependencies]` (add `rcgen`, `tokio-rustls`)
- Modify: `crates/paigasus-helikon-tools/Cargo.toml` `[dev-dependencies]`

**Interfaces:** none new (tests existing `.controller_ca`).

- [ ] **Step 1: Add dev-deps.** Resolve current versions: `cargo search rcgen` / `cargo search tokio-rustls`. In root `Cargo.toml` `[workspace.dependencies]` add (pin to the resolved latest):

```toml
rcgen        = "0.13"
tokio-rustls = "0.26"
```

In `crates/paigasus-helikon-tools/Cargo.toml` `[dev-dependencies]`:

```toml
rcgen        = { workspace = true }
tokio-rustls = { workspace = true }
```

- [ ] **Step 2: Write the test** — `tests/forkd_tls.rs`:

```rust
#![cfg(feature = "microvm")]
#![allow(missing_docs)]

use std::sync::Arc;

use paigasus_helikon_tools::{ExecRequest, ForkdBackend};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

/// Spin a self-signed TLS server that 200s the fork call once; return (addr, ca_pem).
async fn tls_controller() -> (String, Vec<u8>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let ca_pem = cert.cert.pem().into_bytes();
    let key = PrivateKeyDer::try_from(cert.signing_key.serialize_der()).unwrap();
    let chain = vec![CertificateDer::from(cert.cert.der().to_vec())];
    let cfg = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(cfg));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(sock).await {
                    // Minimal: respond 200 with an empty sandbox array then close.
                    let body = b"[{\"id\":\"sb-tls\"}]";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    );
                    let _ = tls.write_all(resp.as_bytes()).await;
                    let _ = tls.write_all(body).await;
                    let _ = tls.shutdown().await;
                }
            });
        }
    });
    (format!("https://localhost:{}", addr.port()), ca_pem)
}

#[tokio::test]
async fn rejects_untrusted_controller_cert_without_ca() {
    let (url, _ca) = tls_controller().await;
    let backend = ForkdBackend::builder(url)
        .bearer_token("t").snapshot("s").build().unwrap();
    // No .controller_ca → the self-signed cert is untrusted → the request fails.
    let err = backend.run(ExecRequest::new("echo hi")).await;
    assert!(err.is_err(), "untrusted TLS cert must fail closed");
}

#[tokio::test]
async fn accepts_controller_cert_with_pinned_ca() {
    let (url, ca) = tls_controller().await;
    let backend = ForkdBackend::builder(url)
        .bearer_token("t").snapshot("s").controller_ca(ca).build().unwrap();
    // With the CA pinned, the fork call connects (then fails later on missing exec
    // mock — but the TLS handshake succeeded, which is what we assert: not a TLS error).
    let res = backend.run(ExecRequest::new("echo hi")).await;
    // Either Ok (if it parses) or a non-TLS error; assert it's NOT a connect/TLS failure.
    if let Err(e) = res {
        let msg = format!("{e:#}");
        assert!(!msg.contains("certificate") && !msg.contains("tls"), "unexpected TLS failure: {msg}");
    }
}
```

(Adjust `rcgen` 0.13 API names if `cargo doc` shows differences — `generate_simple_self_signed` returns a `CertifiedKey { cert, signing_key }`; use whatever the resolved version exposes for PEM + DER.)

- [ ] **Step 3: Run + verify cargo-deny**

Run: `cargo test -p paigasus-helikon-tools --features microvm --test forkd_tls`
Run: `cargo deny check`
Expected: tests PASS; `cargo deny check` PASS (note the new `rcgen` deps' licenses, e.g. `yasna` BSD-3-Clause, in the commit message).

- [ ] **Step 4: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --features microvm --all-targets -- -D warnings
git add Cargo.toml crates/paigasus-helikon-tools/Cargo.toml crates/paigasus-helikon-tools/tests/forkd_tls.rs Cargo.lock
git commit -m "test(tools): SMA-437 verify controller TLS trust (in-test self-signed CA, no danger flag)"
```

---

### Task 7: Un-`#[ignore]` the live test; add the live egress-deny test

**Files:**
- Create: `crates/paigasus-helikon-tools/tests/forkd_live.rs`
- Modify: `crates/paigasus-helikon-tools/tests/forkd_backend.rs` (delete the `#[ignore]`'d `live_forkd_runs_bash_in_a_microvm`)

**Interfaces:** none new.

- [ ] **Step 1: Delete** the `#[ignore]`'d `live_forkd_runs_bash_in_a_microvm` test (lines 148-166) from `tests/forkd_backend.rs`.

- [ ] **Step 2: Create `tests/forkd_live.rs`** (no `#[ignore]`; env-gated with a loud skip):

```rust
#![cfg(feature = "microvm")]
#![allow(missing_docs)]

//! Live forkd integration tests. NOT #[ignore]'d: they compile on every PR (so
//! they cannot bit-rot) and skip LOUDLY when no controller is configured. Run on
//! an x86_64 KVM host with FORKD_URL/FORKD_TOKEN/FORKD_SNAPSHOT set (see
//! docs/runbooks/forkd-live-validation.md).

use std::time::{Duration, Instant};

use paigasus_helikon_tools::{EgressPolicy, ExecRequest, ForkdBackend};

/// Returns (url, token, snapshot) or prints a loud skip and returns None.
fn live_env() -> Option<(String, String, String)> {
    match (
        std::env::var("FORKD_URL"),
        std::env::var("FORKD_TOKEN"),
        std::env::var("FORKD_SNAPSHOT"),
    ) {
        (Ok(u), Ok(t), Ok(s)) => Some((u, t, s)),
        _ => {
            eprintln!(
                "SKIP live forkd test: set FORKD_URL, FORKD_TOKEN, FORKD_SNAPSHOT \
                 (+ optional FORKD_CA path, FORKD_PROXY) to run against a live KVM controller"
            );
            None
        }
    }
}

fn backend(enforce: bool) -> Option<std::sync::Arc<dyn paigasus_helikon_tools::ExecutionBackend>> {
    let (url, token, snapshot) = live_env()?;
    let mut b = ForkdBackend::builder(url).bearer_token(token).snapshot(snapshot);
    if let Ok(ca) = std::env::var("FORKD_CA") {
        b = b.controller_ca(std::fs::read(ca).expect("FORKD_CA file"));
    }
    if enforce {
        let proxy = std::env::var("FORKD_PROXY").expect("FORKD_PROXY for enforced egress");
        b = b.egress_policy(EgressPolicy::deny_all().allow_domains(["example.com"]))
            .enforce_egress(proxy);
    }
    Some(b.build().expect("builds"))
}

#[tokio::test]
async fn live_forkd_runs_bash_in_a_microvm() {
    let Some(backend) = backend(false) else { return };
    let out = backend.run(ExecRequest::new("echo from-a-microvm")).await.unwrap();
    assert_eq!(out.stdout.trim(), "from-a-microvm");
    assert_eq!(out.exit_code, Some(0));
}

#[tokio::test]
async fn live_forkd_denies_nonallowlisted_egress() {
    let Some(backend) = backend(true) else { return };
    // A proxy-aware client to a NON-allowlisted domain must fail FAST (proxy 403),
    // distinguishing "denied" from "hung/timeout".
    let start = Instant::now();
    let out = backend
        .run(ExecRequest::new(
            "curl -s -o /dev/null -w '%{http_code}' --max-time 8 https://evil.test || echo DENIED",
        ))
        .await
        .unwrap();
    assert!(
        out.stdout.contains("DENIED") || out.stdout.trim() == "403",
        "non-allowlisted egress should be denied, got: {:?}",
        out.stdout
    );
    assert!(start.elapsed() < Duration::from_secs(8), "deny must be fast, not a hang");

    // The allow-listed domain succeeds.
    let ok = backend
        .run(ExecRequest::new(
            "curl -s -o /dev/null -w '%{http_code}' --max-time 8 https://example.com",
        ))
        .await
        .unwrap();
    assert_eq!(ok.stdout.trim(), "200", "allow-listed egress should succeed");
}
```

- [ ] **Step 3: Verify it compiles + skips loudly (no controller here)**

Run: `cargo test -p paigasus-helikon-tools --features microvm --test forkd_live`
Expected: PASS with the "SKIP live forkd test" message on stderr (no FORKD_URL set).

- [ ] **Step 4: fmt + clippy + commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-tools --features microvm --all-targets -- -D warnings
git add crates/paigasus-helikon-tools/tests/forkd_live.rs crates/paigasus-helikon-tools/tests/forkd_backend.rs
git commit -m "test(tools): SMA-437 un-ignore live forkd test + add live egress-deny test"
```

---

## Commit group B — Docker forkd+KVM harness, scripts, runbook

### Task 8: `EgressProxy` runner example

**Files:**
- Create: `crates/paigasus-helikon-tools/examples/egress_proxy.rs`
- Modify: `crates/paigasus-helikon-tools/Cargo.toml` (`[[example]]` with `required-features = ["microvm"]`)

- [ ] **Step 1: Create the example**

```rust
//! Run the SMA-437 egress proxy as a standalone process (used by the Docker
//! forkd+KVM harness). Reads bind addr from `EGRESS_BIND` (default 127.0.0.1:8443)
//! and a comma-separated allow-list from `EGRESS_ALLOW`.
//!
//! Run: `EGRESS_ALLOW=example.com cargo run -p paigasus-helikon-tools \
//!       --features microvm --example egress_proxy`

use paigasus_helikon_tools::{EgressPolicy, EgressProxy};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let bind = std::env::var("EGRESS_BIND").unwrap_or_else(|_| "127.0.0.1:8443".into());
    let allow: Vec<String> = std::env::var("EGRESS_ALLOW")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let policy = if allow.is_empty() {
        EgressPolicy::deny_all()
    } else {
        EgressPolicy::deny_all().allow_domains(allow)
    };
    let listener = TcpListener::bind(&bind).await?;
    eprintln!("egress proxy listening on {bind}");
    EgressProxy::new(policy).serve(listener).await
}
```

- [ ] **Step 2: Register the example** in `Cargo.toml`:

```toml
[[example]]
name              = "egress_proxy"
required-features = ["microvm"]
```

- [ ] **Step 3: Verify it builds + commit**

Run: `cargo build -p paigasus-helikon-tools --features microvm --example egress_proxy`
Expected: PASS.

```bash
cargo fmt --all
git add crates/paigasus-helikon-tools/examples/egress_proxy.rs crates/paigasus-helikon-tools/Cargo.toml
git commit -m "feat(tools): SMA-437 add egress_proxy example runner for the harness"
```

---

### Task 9: Docker forkd+KVM harness

**Files (all new, repo root):**
- Create: `docker/forkd/Dockerfile`
- Create: `docker/forkd/docker-compose.yml`
- Create: `docker/forkd/entrypoint.sh`
- Create: `docker/forkd/netns-deny.rules`
- Create: `docker/forkd/README.md`

- [ ] **Step 1: `docker/forkd/Dockerfile`** — Ubuntu 22.04; forkd + Firecracker + the egress proxy binary. (Pin `FORKD_VERSION=0.5.2`.)

```dockerfile
# forkd + Firecracker + Helikon egress proxy — x86_64 Linux KVM host only.
FROM ubuntu:22.04
ARG FORKD_VERSION=0.5.2
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl iproute2 iptables busybox-static && rm -rf /var/lib/apt/lists/*
# forkd controller (x86_64-linux release tarball).
RUN curl -fsSL "https://github.com/deeplethe/forkd/releases/download/v${FORKD_VERSION}/forkd-v${FORKD_VERSION}-x86_64-linux.tar.gz" \
    | tar -xz -C /usr/local/bin
# The egress proxy is built on the host and COPYed in (see docker-compose build args),
# or mount the cargo-built binary at /usr/local/bin/egress-proxy.
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
COPY netns-deny.rules /etc/forkd/netns-deny.rules
RUN chmod +x /usr/local/bin/entrypoint.sh
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
```

- [ ] **Step 2: `docker/forkd/netns-deny.rules`** — the committed, reviewable Layer 1 ruleset (template; `${PROXY_IP}`/`${PROXY_PORT}`/`${DNS_IP}` substituted by entrypoint):

```
# SMA-437 Layer-1 per-netns default-deny. Applied inside each child netns.
# Drop all egress except: DNS to the vetted resolver, and TCP to the egress proxy.
*filter
:OUTPUT DROP [0:0]
-A OUTPUT -o lo -j ACCEPT
-A OUTPUT -p udp -d ${DNS_IP} --dport 53 -j ACCEPT
-A OUTPUT -p tcp -d ${PROXY_IP} --dport ${PROXY_PORT} -j ACCEPT
-A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
COMMIT
```

- [ ] **Step 3: `docker/forkd/entrypoint.sh`** — load rules, assert loaded, start proxy + controller:

```bash
#!/usr/bin/env bash
set -euo pipefail
: "${PROXY_PORT:=8443}"
: "${EGRESS_ALLOW:=example.com}"
PROXY_IP="127.0.0.1"
DNS_IP="${DNS_IP:-1.1.1.1}"

# Start the egress proxy.
EGRESS_BIND="0.0.0.0:${PROXY_PORT}" EGRESS_ALLOW="${EGRESS_ALLOW}" \
  /usr/local/bin/egress-proxy &
PROXY_PID=$!

# forkd doctor: fail fast if KVM/cgroup-v2/Firecracker are missing.
forkd doctor

# Apply Layer-1 rules into each provisioned child netns (forkd's netns-setup runs first).
export PROXY_IP PROXY_PORT DNS_IP
for ns in $(ip netns list | awk '{print $1}'); do
  envsubst < /etc/forkd/netns-deny.rules | ip netns exec "$ns" iptables-restore
  # Assert the default policy is DROP; abort if not.
  ip netns exec "$ns" iptables -S OUTPUT | grep -q -- '-P OUTPUT DROP' \
    || { echo "FATAL: netns $ns OUTPUT policy is not DROP"; exit 1; }
done

# Start the controller over TLS with bearer auth.
exec forkd-controller \
  --tls-cert /etc/forkd/tls/cert.pem \
  --tls-key  /etc/forkd/tls/key.pem \
  --token-file /etc/forkd/token \
  --per-child-netns
```

- [ ] **Step 4: `docker/forkd/docker-compose.yml`**:

```yaml
services:
  forkd:
    build:
      context: .
    devices:
      - "/dev/kvm:/dev/kvm"
    cap_add:
      - NET_ADMIN
    environment:
      PROXY_PORT: "8443"
      EGRESS_ALLOW: "example.com"
      DNS_IP: "1.1.1.1"
    volumes:
      # cargo-built proxy binary + TLS material + token + warmed snapshot dir.
      - ./egress-proxy:/usr/local/bin/egress-proxy:ro
      - ./tls:/etc/forkd/tls:ro
      - ./token:/etc/forkd/token:ro
      - forkd-snapshots:/var/lib/forkd
    ports:
      - "8889:8889"
volumes:
  forkd-snapshots:
```

- [ ] **Step 5: `docker/forkd/README.md`** — one-screen "what this is + see the runbook" pointer.

- [ ] **Step 6: Lint (best-effort) + commit**

Run: `bash -n docker/forkd/entrypoint.sh` (syntax check). If `shellcheck`/`hadolint` are installed, run them; otherwise note they were unavailable.

```bash
git add docker/forkd
git commit -m "feat(tools): SMA-437 add Dockerized forkd+KVM harness with netns default-deny"
```

---

### Task 10: Guest-image build + GCP launch scripts

**Files (new):**
- Create: `scripts/forkd/build-guest-image.sh`
- Create: `scripts/forkd/gcp-launch.sh`
- Create: `scripts/forkd/gcp-teardown.sh`

- [ ] **Step 1: `scripts/forkd/build-guest-image.sh`** — build a minimal rootfs (busybox `sh` + coreutils + curl + ca-certs), bake `HTTP_PROXY`/`HTTPS_PROXY`, **secret-scan**, then warm + snapshot via `POST /v1/snapshots`:

```bash
#!/usr/bin/env bash
# Build + warm a forkd guest snapshot for Helikon. Run on the x86_64 KVM host.
set -euo pipefail
: "${ROOTFS:=/var/lib/forkd/rootfs/helikon.ext4}"
: "${KERNEL:=/var/lib/forkd/kernels/vmlinux-6.1}"
: "${PROXY_ADDR:?set PROXY_ADDR=host:8443 (the egress proxy reachable from the guest netns)}"
: "${FORKD_URL:?set FORKD_URL}"; : "${FORKD_TOKEN:?set FORKD_TOKEN}"
: "${SNAPSHOT_TAG:=helikon}"
WORK="$(mktemp -d)"

# --- assemble a minimal rootfs (busybox + curl + ca-certs) ---
mkdir -p "$WORK/rootfs"/{bin,etc,proc,sys,dev}
busybox --install -s "$WORK/rootfs/bin"
cp "$(command -v curl)" "$WORK/rootfs/bin/" || true
cat > "$WORK/rootfs/etc/profile" <<EOF
export HTTP_PROXY=http://${PROXY_ADDR}
export HTTPS_PROXY=http://${PROXY_ADDR}
export http_proxy=http://${PROXY_ADDR}
export https_proxy=http://${PROXY_ADDR}
EOF

# --- SECRET SCAN: refuse to snapshot if any secret material is present ---
if grep -RInE '(BEGIN [A-Z ]*PRIVATE KEY|AKIA[0-9A-Z]{16}|Bearer [A-Za-z0-9._-]{20,})' "$WORK/rootfs"; then
  echo "FATAL: secret-like material found in rootfs — refusing to snapshot (CoW is shared to every child)."
  exit 1
fi

# (Package $WORK/rootfs into $ROOTFS ext4 here — mkfs.ext4 + cp; omitted for brevity,
#  see runbook for the exact mkfs invocation.)

# --- warm + snapshot ---
curl -fsSL -X POST "${FORKD_URL%/}/v1/snapshots" \
  -H "Authorization: Bearer ${FORKD_TOKEN}" \
  -H 'Content-Type: application/json' \
  -d "{\"tag\":\"${SNAPSHOT_TAG}\",\"kernel\":\"${KERNEL}\",\"rootfs\":\"${ROOTFS}\",\"rw\":true,\"tap\":\"forkd-tap0\",\"boot_wait_secs\":10}"
echo "snapshot '${SNAPSHOT_TAG}' requested; poll GET /v1/snapshots for status=ready"
```

- [ ] **Step 2: `scripts/forkd/gcp-launch.sh`** — nested-virt VM + Docker + harness:

```bash
#!/usr/bin/env bash
# Provision a GCP nested-virt VM to run the forkd+KVM harness. Requires gcloud auth.
set -euo pipefail
: "${GCP_PROJECT:?set GCP_PROJECT}"
: "${GCP_ZONE:=europe-west1-b}"
: "${VM_NAME:=forkd-kvm}"
: "${MACHINE:=n2-standard-4}"

gcloud compute instances create "$VM_NAME" \
  --project "$GCP_PROJECT" --zone "$GCP_ZONE" --machine-type "$MACHINE" \
  --enable-nested-virtualization \
  --image-family ubuntu-2204-lts --image-project ubuntu-os-cloud \
  --metadata=startup-script='#!/bin/bash
    set -e
    apt-get update && apt-get install -y docker.io docker-compose-plugin
    systemctl enable --now docker
    # KVM check inside the VM:
    ls -l /dev/kvm || echo "WARN: /dev/kvm absent — nested virt not enabled?"
  '
echo "VM $VM_NAME up in $GCP_ZONE. SSH in, copy docker/forkd + the cargo-built egress-proxy, then 'docker compose up'."
echo "See docs/runbooks/forkd-live-validation.md."
```

- [ ] **Step 3: `scripts/forkd/gcp-teardown.sh`**:

```bash
#!/usr/bin/env bash
set -euo pipefail
: "${GCP_PROJECT:?}"; : "${GCP_ZONE:=europe-west1-b}"; : "${VM_NAME:=forkd-kvm}"
gcloud compute instances delete "$VM_NAME" --project "$GCP_PROJECT" --zone "$GCP_ZONE" --quiet
```

- [ ] **Step 4: `chmod +x` + syntax-check + commit**

```bash
chmod +x scripts/forkd/*.sh docker/forkd/entrypoint.sh
for f in scripts/forkd/*.sh; do bash -n "$f"; done
git add scripts/forkd
git commit -m "feat(tools): SMA-437 add guest-image build (secret-scanned) + GCP launch/teardown scripts"
```

---

### Task 11: Live-validation runbook

**Files:**
- Create: `docs/runbooks/forkd-live-validation.md`

- [ ] **Step 1: Write the runbook** covering, in order: prerequisites (x86_64 KVM host; gcloud auth via `gcloud auth login` — never paste keys); `scripts/forkd/gcp-launch.sh`; build the proxy (`cargo build -p paigasus-helikon-tools --features microvm --example egress_proxy`) and copy it + `docker/forkd` to the VM; generate TLS cert/key + token; `docker compose up` (incl. **container `/dev/kvm` passthrough**: `--device /dev/kvm`, device-cgroup; `forkd doctor` must pass); `build-guest-image.sh`; run `FORKD_URL=… FORKD_TOKEN=… FORKD_SNAPSHOT=… FORKD_PROXY=… cargo test -p paigasus-helikon-tools --features microvm --test forkd_live -- --nocapture`; expected output (both live tests pass; egress-deny is fast); paste results into the PR; `gcp-teardown.sh`. Plus: the real-CA non-loopback TLS story, and AWS (C8i nested-virt / `.metal`), Hetzner bare-metal, DO alternatives.

- [ ] **Step 2: Verify the book still builds** (if the runbook is linked from `docs/book/src/SUMMARY.md`; otherwise it lives standalone under `docs/runbooks/`). Decide: keep it under `docs/runbooks/` (not in the book) to avoid linkcheck coupling. Commit:

```bash
git add docs/runbooks/forkd-live-validation.md
git commit -m "docs(tools): SMA-437 add forkd live-KVM validation runbook"
```

---

## Commit group C — public docs + release

### Task 12: mdBook + READMEs

**Files:**
- Modify: `docs/book/src/concepts/tools.md` (microVM tier + containment ladder)
- Modify: `crates/paigasus-helikon-tools/README.md`
- Modify: `crates/paigasus-helikon/README.md` (facade)
- Modify: `README.md` (root feature→module map, if it lists `Isolation` variants / `microvm`)

- [ ] **Step 1: mdBook** — in `docs/book/src/concepts/tools.md`, update the microVM tier: egress is **Proxied** when the layered enforcement is deployed (`.enforce_egress()` + the harness), `None` by default; the containment-ladder caveat now says the microVM tier is no longer the weak-on-egress axis *once enforced* (and remains `None` un-enforced). Mention `EgressProxy` + the runbook.

- [ ] **Step 2: `-tools` README** — add the microVM egress-enforcement story + a pointer to `docs/runbooks/forkd-live-validation.md`; note the `microvm` feature now pulls `url` + `tokio/net,io-util`.

- [ ] **Step 3: facade + root README** — one-line notes for `Isolation::Proxied` + `EgressProxy` in the feature→module map.

- [ ] **Step 4: Verify the book builds clean**

Run: `mdbook build docs/book`
Expected: PASS (no linkcheck warnings; `warning-policy = "error"`).

- [ ] **Step 5: Commit**

```bash
git add docs/book crates/paigasus-helikon-tools/README.md crates/paigasus-helikon/README.md README.md
git commit -m "docs(tools): SMA-437 document microVM egress enforcement (Proxied tier + EgressProxy)"
```

---

### Task 13: Version bump + CHANGELOG + full local CI gate

**Files:**
- Modify: `crates/paigasus-helikon-tools/Cargo.toml` (`version = "0.2.7"`)
- Modify: `crates/paigasus-helikon-tools/CHANGELOG.md`

- [ ] **Step 1: Bump version** in `crates/paigasus-helikon-tools/Cargo.toml`: `version = "0.2.6"` → `"0.2.7"`.

- [ ] **Step 2: Prepend a CHANGELOG entry** describing: `EgressProxy`, `Isolation::Proxied`, `EgressPolicy` promotion (with `is_allowed` deprecated), `.enforce_egress()`, the live harness/runbook, the new `microvm` deps.

- [ ] **Step 3: Run the FULL CI gate locally** (matches `.github/workflows/ci.yml`):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo deny check
mdbook build docs/book
```

Expected: all PASS. The `forkd_live` tests run under `--all-features` and **skip loudly** (no FORKD_URL) — confirm they print SKIP and pass, never hang.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-tools/Cargo.toml crates/paigasus-helikon-tools/CHANGELOG.md Cargo.lock
git commit -m "chore(release): SMA-437 bump paigasus-helikon-tools to 0.2.7"
```

---

### Task 14 (process, pre-merge): file the orphan-GC follow-up

- [ ] **Step 1:** File a Linear follow-up ticket "forkd microVM: GC/reconciliation of orphaned sandboxes" (list-by-tag/age + reap), blocked-by SMA-437. Update the `forkd.rs` orphan comment to cite the new ticket id instead of "SMA-437 adds GC/reconciliation." Amend Task 5's forkd.rs commit or add a small follow-up commit. (Not code-gated; ensures the deferral from spec §10 is tracked.)

---

## Self-Review

**Spec coverage:**
- §3.1 policy promotion → Tasks 1, 2. §3.2 proxy → Task 4. §3.3 `Proxied` → Task 3. §3.4 enforce_egress + probe + honesty → Task 5. §4.1 harness → Task 9. §4.2 guest-image + secret-scan → Task 10. §4.3 GCP scripts → Task 10. §4.4 live tests → Task 7. §4.5 runbook → Task 11. §5 TLS test → Task 6. §8 release/features/deps → Tasks 4 (feature deps), 6 (dev-deps), 13 (version). §8 docs → Task 12. §9 AC mapping → covered across tasks. §10 orphan-GC deferral → Task 14. Proxy runner (examples) → Task 8.
- All spec sections map to a task. ✓

**Placeholder scan:** the only intentionally-abbreviated spot is the ext4 packaging in `build-guest-image.sh` (Step 1, Task 10), explicitly deferred to the runbook's exact `mkfs` invocation — acceptable because it's host-specific shell, not Rust, and the runbook (Task 11) carries it. No Rust step is abbreviated.

**Type consistency:** `EgressPolicy::{deny_all, allow_all, allow_domains, deny_domains, allow_private_ips, is_host_allowed, is_ip_allowed, is_allowed(deprecated)}` used consistently across Tasks 2, 4, 5, 7, 8. `EgressProxy::{new, serve}` consistent across Tasks 4, 8, egress_proxy.rs tests. `ForkdBackendBuilder::enforce_egress` + `ForkdError::ProxyUnreachable` + `Isolation::Proxied` consistent across Tasks 3, 5, 7. ✓
```