# SMA-437 — `paigasus-helikon-tools`: forkd microVM — egress enforcement + live-KVM validation

**Status:** draft — pending adversarial challenge + GATE 1 approval
**Ticket:** [SMA-437](https://linear.app/smaschek/issue/SMA-437/paigasus-helikon-tools-forkd-microvm-live-kvm-validation-egress)
**Milestone:** Composition & Extensibility
**Branch:** `feature/sma-437-paigasus-helikon-tools-forkd-microvm-live-kvm-validation`
**Date:** 2026-06-22
**Builds on:** [SMA-416](https://linear.app/smaschek/issue/SMA-416) (the `ForkdBackend` skeleton + spike note) · promotes [SMA-412](https://linear.app/smaschek/issue/SMA-412) (web domain/SSRF policy) · completes the [SMA-413](https://linear.app/smaschek/issue/SMA-413) §11 egress-proxy follow-up

---

## 1. Summary

SMA-416 shipped a compiling, mock-tested `ForkdBackend` — a portable REST client of
the forkd Firecracker controller — that *carries* an `EgressPolicy` but does **not**
enforce it, and honestly reports `guarantees().network = Isolation::None`. This
ticket makes the microVM tier **actually contain egress** and **actually validate
end-to-end on KVM**. It delivers, in one PR:

1. **Egress enforcement (the CI-provable half).** Promote SMA-412's host/IP/SSRF
   policy to a **public shared type** (`net::EgressPolicy`), build a **CONNECT
   egress proxy** (`net::EgressProxy`) that enforces the domain allow/deny policy +
   the private-IP (SSRF) block, add the `Isolation::Proxied` variant, and upgrade
   `ForkdBackend::guarantees().network` from `None` to `Proxied` when the backend is
   built in enforced-egress mode. **All of this is pure async Rust, unit/loopback
   tested green in CI — no KVM required.**

2. **Live-KVM validation (the operator-run half).** A **cloud-agnostic, Dockerized
   forkd+KVM harness** (`docker compose` running forkd with `--device /dev/kvm` + the
   egress proxy as a sidecar + per-VM netns default-deny iptables), a **guest-image
   build script**, a **GCP nested-virtualization reference launch script** (the
   chosen reference cloud; AWS/Hetzner/DO documented as alternatives), an **env- and
   feature-gated live integration test**, and a **runbook**. The live fork→exec→
   destroy + egress-deny path is exercised on a real x86_64 KVM host.

**Hard environment constraint (the reason for the two-half split).** The dev host is
macOS on Apple Silicon (arm64) and GitHub-hosted CI has no `/dev/kvm`. Verified
empirically: a container on this host has **no `/dev/kvm` and no `vmx`/`svm` CPU
flag** (Docker Desktop's linuxkit VM exposes no nested virtualization), and forkd
ships **x86_64-Linux-only** — so KVM cannot run here at all. Everything KVM-dependent
is therefore authored + lint-validated here and **executed on an x86_64 KVM Linux
host** (per Sven: a **GCP nested-virtualization VM**, provisioned by the launch
script, run live this work-cycle once the code exists; credentials supplied via
interactive `gcloud auth login`, never pasted into chat).

`-tools` is at `0.2.6`; this is an **additive** change (new `#[non_exhaustive]` enum
variant + new public types in a feature-gated module) → patch bump **`0.2.7`**,
normal release-plz flow, **no `paigasus-helikon-core` change**.

## 2. Decisions resolved in brainstorming

1. **Maximal scope, no deferral.** All four ticket ACs' code/scripts/docs land in
   this one PR. Nothing is punted to a new ticket.
2. **Live validation = a Dockerized forkd+KVM harness on GCP nested-virt**, exercised
   via an env-gated live test + a runbook. **No new self-hosted GitHub Actions
   workflow** (Sven: "no CI dependency"). The harness is cloud-agnostic; GCP is the
   reference provisioner.
3. **Egress enforcement is layered** (the SMA-416 §6 / SMA-413 §11 model):
   **Layer 1 — per-VM netns default-deny** (host iptables in each child netns: DROP
   all egress except the route to the proxy) and **Layer 2 — a CONNECT proxy
   enforcing the domain policy** (HTTP/S). Layer 1 is host config the harness
   deploys + the runbook documents (not Rust, not CI-testable here). Layer 2 is *our*
   Rust code and **is** CI-testable on loopback — it is the artifact that proves the
   AC "a non-allowlisted egress attempt is denied."
4. **`guarantees().network` stays honest** (the SMA-413 H1 rule). The upgrade to
   `Proxied` is **gated on the backend being built in enforced-egress mode** (an
   explicit builder opt-in that *attests* the operator has deployed both layers — the
   same trust model the other tiers use for the kernel/hypervisor). Default (no
   enforced egress) remains `Isolation::None`. We never advertise containment we are
   not configured to enforce.
5. **Promotion lives in `-tools`, not `core`.** The shared policy is shared between
   the `web` tools and the `microvm` backend/proxy — both in `paigasus-helikon-tools`
   — so a crate-internal `net` module promoted to `pub` suffices. **No core change →
   no ascend ritual, no manual facade bump.**
6. **The CONNECT proxy is purpose-built Rust, not Squid/tinyproxy.** Only our own
   proxy can enforce *our* exact `EgressPolicy` semantics (sub-domain matching,
   deny-beats-allow) and reuse `ip_blocked` for the SSRF/rebinding guard. ~250 lines
   of tokio; it is also the testable artifact for the egress AC.

## 3. Egress enforcement (the CI-provable code)

### 3.1 Promote the SMA-412 policy to a public shared type

Today SMA-412's `host_allowed` / `ip_blocked` / `GuardedResolver` / `ssrf_check` /
`build_client` are `pub(crate)` in `src/web/http.rs`, and `forkd.rs` carries a
**second, duplicated** domain matcher (`EgressPolicy::is_allowed`). We unify both
behind one public type.

**New module `src/net/`**, compiled under `#[cfg(any(feature = "web", feature =
"microvm"))]`:

- **`net::policy`** — the promoted policy:
  - **`pub struct EgressPolicy`** (the single shared type): domain `allow`/`deny`
    lists **plus** an `allow_private_ips` toggle. Builder-style: `deny_all()`,
    `allow_all()`, `allow_domains(..)`, `deny_domains(..)`, `allow_private_ips(bool)`.
    Checks: `is_host_allowed(&str) -> bool` (the promoted `host_allowed` logic —
    sub-domain-aware, case-insensitive, trailing-dot-insensitive, deny-beats-allow)
    and `is_ip_allowed(IpAddr) -> bool` (wrapping the promoted `ip_blocked`
    classifier, honoring `allow_private_ips`).
  - **`pub fn ip_blocked(IpAddr) -> bool`** and the SSRF range helpers — promoted
    verbatim (the existing exhaustive v4/v6 range coverage and tests move with them).
  - **`pub struct GuardedResolver`** + `pub(crate) fn build_client(..)` +
    `pub(crate) async fn ssrf_check(..)` — promoted; `GuardedResolver` now resolves
    through `EgressPolicy::is_ip_allowed`. Made `pub` because the cloud `E2bBackend`
    sibling and other reqwest controllers can reuse it.
- **`web` refactor (no public-API change).** `web/http.rs` shrinks to re-exports from
  `net`; `web/fetch.rs`, `web/search.rs`, `web/backends/*` import from `net`.
  `WebFetchToolBuilder`'s public methods (`allow_domains` / `deny_domains` /
  `allow_private_ips`) are **unchanged**; internally they now build/consult an
  `EgressPolicy`. The existing `web` tests stay green (the moved tests are the
  regression guard).
- **`forkd.rs`** drops its duplicated `EgressPolicy` definition and re-uses
  `net::EgressPolicy`. `paigasus_helikon_tools::EgressPolicy` remains the canonical
  crate-root path (the re-export simply moves from `exec` to `net`), so there is **no
  external breakage**.

### 3.2 The CONNECT egress proxy

**New module `src/net/proxy`** (under `#[cfg(feature = "microvm")]`):

- **`pub struct EgressProxy`** constructed from an `EgressPolicy`. `pub async fn
  serve(self, listener: tokio::net::TcpListener) -> io::Result<()>` accepts
  connections and, per connection:
  - **`CONNECT host:port` (HTTPS tunneling — the primary path):** parse the request
    line + headers; check `policy.is_host_allowed(host)`; resolve the host and check
    every address with `policy.is_ip_allowed(ip)` (closes the DNS-rebinding window,
    pinning the tunnel to a vetted address); on pass, reply `200 Connection
    Established` and `tokio::io::copy_bidirectional` the byte stream; on fail, reply
    `403 Forbidden` and close. **No upstream connection is made for a denied host.**
  - **Absolute-URI plain HTTP (`GET http://host/… HTTP/1.1`):** enforce the `Host`
    against the policy, then forward via a `build_client`-built reqwest client (reuses
    the guarded resolver). Handled so plain HTTP cannot bypass enforcement once netns
    default-deny routes all egress here.
- **Bearer/secret hygiene:** the proxy logs hostnames + verdicts (allow/deny) but
  never request bodies or `Authorization` headers.
- **Reusability:** the proxy takes an `EgressPolicy` and a listener — it is
  deployment-agnostic. The Docker harness runs it as a sidecar on the forkd host; a
  test runs it on `127.0.0.1`.

**Why this proves the AC here, without KVM:** the deny decision happens *in the proxy*
before any tunnel — so "non-allowlisted egress is denied" is a pure loopback test
(below, §6).

### 3.3 The `Isolation::Proxied` variant

`Isolation` is `#[non_exhaustive]`; adding a variant is additive/non-breaking
(downstream matches already need a wildcard arm). Doc string:

> *"Egress is filtered by an allow/deny **domain** policy enforced at a CONNECT
> proxy (application layer, HTTP/S). `Proxied` describes the proxied traffic only:
> raw L3/L4 containment depends on the deployment's per-VM netns default-deny, which
> the backend does not itself verify. Read as 'egress is policy-filtered for proxied
> traffic,' not 'all packets are blocked.'"*

This is categorically distinct from `OsKernel` (a seccomp socket-family block) and
from `Virtualized` (a hypervisor boundary).

### 3.4 `ForkdBackend` integration & the honesty model

- New builder opt-in: **`.enforce_egress(EgressEnforcement)`** where
  `EgressEnforcement` records the proxy endpoint the operator has deployed (and the
  `EgressPolicy` it enforces — which must equal the policy the backend carries, so
  declared intent and enforced reality are one value). When set:
  - `guarantees().network` reports **`Isolation::Proxied`** (and the label drops
    "experimental" for the network axis caveat).
  - `build()` **optionally health-checks** the proxy endpoint reachability (a cheap
    liveness probe) so a fat-fingered endpoint fails closed at construction rather
    than silently un-enforced.
- When `.enforce_egress(..)` is **not** set, `guarantees().network` stays
  `Isolation::None` exactly as today.
- **Honesty caveat (documented on the method):** the backend cannot verify the host's
  netns iptables; `Proxied` *attests* that the operator deployed Layer 1 + Layer 2
  (which the harness/runbook do). This mirrors how `OsSandboxBackend` trusts the
  kernel applied the seccomp filter. The default is the safe lie-free `None`.

## 4. Live-KVM validation (the operator-run code + procedure)

Authored + lint-validated here; **executed on an x86_64 GCP nested-virt VM.**

### 4.1 Dockerized forkd+KVM harness — `docker/forkd/`

- **`Dockerfile`** — Ubuntu 22.04 base; installs the forkd `v0.5.2-x86_64-linux`
  release + a Firecracker binary; builds + installs the `EgressProxy` (a thin
  `--bin egress-proxy` example/bin, or the harness invokes it via the test crate);
  bakes the controller TLS cert/key + bearer token file paths.
- **`docker-compose.yml`** — runs the container with `--device /dev/kvm`, the netns
  capabilities (`--cap-add=NET_ADMIN`, `--sysctl` as needed), publishes the
  controller port, and runs the egress proxy sidecar. `forkd doctor` runs at start to
  fail fast if KVM/cgroup-v2/Firecracker are missing.
- **`entrypoint.sh`** — provisions per-child netns (forkd's `netns-setup.sh`), applies
  **Layer 1 default-deny iptables** in each netns (DROP egress except to the proxy),
  starts `forkd-controller` with `--tls-cert/--tls-key/--token-file`, starts the
  proxy.

### 4.2 Guest-image build — `scripts/forkd/build-guest-image.sh`

Builds a minimal guest rootfs (kernel + init + `/bin/sh` + coreutils, e.g. busybox)
with **`HTTP_PROXY`/`HTTPS_PROXY` baked into the guest profile** pointing at the proxy
(required because forkd's exec endpoint has **no per-call env** — confirmed against
`docs/API.md`), then warms + snapshots it via `POST /v1/snapshots`. **No secrets are
baked in** (the CoW-shared-state warning from SMA-416 §3.4 — every child inherits the
warmed parent). The script documents who runs it (the operator, on the KVM host).

### 4.3 GCP nested-virt launch — `scripts/forkd/gcp-launch.sh` + `gcp-teardown.sh`

`gcloud compute instances create` with `--enable-nested-virtualization` on a small
x86_64 Intel instance (e.g. `n2-standard-4`, zone `europe-west1-b`), a startup script
that installs Docker + brings the harness up. Teardown deletes the instance. The
runbook lists AWS (C8i nested-virt / `.metal`), Hetzner bare metal, and DO as
documented alternatives.

### 4.4 Env- & feature-gated live tests — `tests/forkd_live.rs`

`#![cfg(feature = "microvm-live")]` (a new **off-by-default, test-only** feature).
Because the live tests are *feature-gated*, they are **absent** from the normal CI
build — **not silently passing** (the SMA-413 honesty rule: a green-because-inactive
sandbox test is worse than no test). On the KVM host the runbook runs
`cargo test -p paigasus-helikon-tools --features microvm,microvm-live -- forkd_live`,
with `FORKD_URL`/`FORKD_TOKEN`/`FORKD_SNAPSHOT` (+ `FORKD_CA`) set. Two tests:

1. **`live_forkd_runs_bash_in_a_microvm`** — `echo from-a-microvm` → asserts stdout +
   `exit_code == 0` (the existing `#[ignore]`'d test, promoted to feature-gated).
2. **`live_forkd_denies_nonallowlisted_egress`** — with a policy allowing only
   `example.com`, a guest command hitting a **non-allowlisted** domain fails/timeouts
   (proxy 403 + netns deny), while a command hitting the allowlisted domain succeeds.
   This is the live proof of the egress AC.

### 4.5 Runbook — `docs/runbooks/forkd-live-validation.md`

End-to-end: `gcloud auth login` (interactive; no secret in chat) → `gcp-launch.sh` →
`build-guest-image.sh` → `docker compose up` → run the `microvm-live` tests with
`FORKD_*` env → observe egress-deny → `gcp-teardown.sh`. Plus the real-CA
non-loopback TLS story (forkd `--tls-cert/--tls-key`; `.controller_ca(pem)`) and the
AWS/Hetzner/DO alternatives.

## 5. Controller TLS trust (end-to-end)

The builder already takes `.controller_ca(pem)` and rejects remote plain-`http`
(`InsecureControllerUrl`). This ticket adds **a loopback TLS integration test**
(`tests/forkd_tls.rs`, no KVM): start a minimal self-signed rustls server (dev-deps
`rcgen` + `tokio-rustls`), then assert (a) a `ForkdBackend` **without**
`.controller_ca` fails the first request with a TLS trust error, and (b) **with**
`.controller_ca(<self-signed cert>)` it succeeds. This proves trust is enforced and
that **`danger_accept_invalid_certs` is never used**. The runbook documents the real
(non-loopback) deployment: forkd served over TLS with a real CA, or the self-signed
CA pinned via `.controller_ca`.

## 6. Tests

| Test (file) | Gate | Proves |
|---|---|---|
| `egress_proxy.rs` — deny non-allowlisted domain → 403 (no upstream) | CI (`microvm`) | AC: non-allowlisted egress denied |
| `egress_proxy.rs` — domain-allowed but resolves to private IP → denied (SSRF) | CI (`microvm`) | rebinding/SSRF guard in the proxy |
| `egress_proxy.rs` — allow_private_ips + allowlisted loopback → bytes tunnel through | CI (`microvm`) | the allow path actually proxies |
| `net::policy` unit tests (moved from `web/http.rs`) | CI | promoted policy unchanged (regression guard) |
| `forkd_backend.rs` mock fork→exec→destroy, timeout/teardown, truncation, 5xx | CI (`microvm`) | existing skeleton contract (unchanged) |
| `guarantees()` — `Proxied` with `.enforce_egress`, `None` without | CI (`microvm`) | honesty model |
| `forkd_tls.rs` — fail without CA / succeed with CA | CI (`microvm`) | TLS trust enforced; no danger flag |
| `forkd_live.rs` — live run + live egress-deny | **`microvm-live`**, env-gated, KVM host | AC: live KVM run + live egress enforcement |

Feature gating keeps the default build and the non-`microvm` matrix from compiling any
of this. cargo-deny stays green (new dev-deps `rcgen`/`tokio-rustls` are MIT/Apache).

## 7. Module layout

```
crates/paigasus-helikon-tools/src/
  net/                       # NEW — cfg(any(feature="web", feature="microvm"))
    mod.rs                   # re-exports
    policy.rs                # EgressPolicy (shared), ip_blocked, GuardedResolver, ssrf_check, build_client
    proxy.rs                 # EgressProxy (CONNECT + absolute-URI HTTP) — cfg(feature="microvm")
  web/http.rs                # shrinks to thin re-exports from net
  web/{fetch,search}.rs, web/backends/*   # import from net
  exec/mod.rs                # + Isolation::Proxied
  exec/forkd.rs              # EgressPolicy from net; .enforce_egress + Proxied guarantee
  lib.rs                     # pub use net::{EgressPolicy, EgressProxy, GuardedResolver, ...}
docker/forkd/                # NEW — Dockerfile, docker-compose.yml, entrypoint.sh
scripts/forkd/               # NEW — build-guest-image.sh, gcp-launch.sh, gcp-teardown.sh
docs/runbooks/forkd-live-validation.md   # NEW runbook
crates/paigasus-helikon-tools/tests/
  egress_proxy.rs, forkd_tls.rs          # NEW (CI)
  forkd_live.rs                          # NEW — cfg(feature="microvm-live")
  forkd_backend.rs                       # existing; #[ignore]'d test moves to forkd_live.rs
```

## 8. Release & docs

- **Version:** `-tools` `0.2.6 → 0.2.7`. Additive `feat(tools)` → patch on 0.x.
  **No core change** → no ascend ritual. Facade cascades via release-plz's
  `dependencies_update` (release-plz performs the `-tools` bump, so the cascade runs).
- **Features:** `microvm = ["dep:reqwest", "dep:url", "tokio/net"]` (adds `url` +
  `tokio/net` for the proxy + shared policy). New **`microvm-live`** test-only feature
  (no deps; gates `tests/forkd_live.rs`). No new facade passthrough feature
  (`tools-microvm` already exists; live tests are internal).
- **Dev-deps:** `rcgen`, `tokio-rustls` (TLS test) — added to `[workspace.dependencies]`
  + `[dev-dependencies]`; licenses MIT/Apache (cargo-deny clean).
- **Docs (same PR, per CLAUDE.md):** mdBook `concepts/tools.md` microVM tier — egress
  now **Proxied** when enforced; update the containment-ladder caveat (egress is no
  longer the weak axis once the proxy is deployed, but is `None` in the un-enforced
  default). `-tools` README (egress enforcement + harness/runbook pointer). Facade +
  root README only if the feature→module map changes (the `Isolation::Proxied`
  variant + `EgressProxy` warrant a one-line note). New runbook page linked from
  SUMMARY if it belongs in the book; otherwise it lives under `docs/runbooks/`.
  `mdbook build docs/book` stays clean (`warning-policy = "error"`). `///` on every new
  `pub` item.

## 9. Acceptance criteria mapping

- ✅ **Guest snapshot image** — `build-guest-image.sh` + runbook (contract from
  SMA-416 §3.4; no secrets baked; HTTP_PROXY baked for egress routing). Operator-run
  on the KVM host.
- ✅ **Live KVM run** — `forkd_live.rs::live_forkd_runs_bash_in_a_microvm`, feature+env
  gated, run on the GCP nested-virt VM via the runbook.
- ✅ **Egress enforcement (layered)** — Layer 1 netns default-deny (harness iptables) +
  Layer 2 `EgressProxy` (CI-tested); `guarantees().network` upgrades `None → Proxied`
  in enforced mode; live-proven by `live_forkd_denies_nonallowlisted_egress`.
- ✅ **Controller TLS trust end-to-end** — `forkd_tls.rs` (CI) + runbook real-CA story;
  no `danger_accept_invalid_certs`.

## 10. Out of scope (YAGNI)

- The `E2bBackend` cloud sibling (verified-compatible, not built).
- Unix-socket controller transport (TCP+TLS only).
- A transparent (iptables-REDIRECT) proxy mode — explicit CONNECT + baked HTTP_PROXY
  is sufficient and matches the AC; transparent mode is a noted future option.
- Per-call stdin/env on `ExecRequest` (forkd exec has no env field anyway).
- A self-hosted GitHub Actions KVM runner / required CI gate for the live path.
- Embedding `forkd-vmm` (the rejected SMA-416 §2.2 seam).

## 11. Risks

| Risk | Mitigation |
|---|---|
| forkd is alpha (pre-1.0 API churn); pinned `v0.5.2` | REST boundary + harness pin one version; API re-verified against `docs/API.md` (fork/exec/destroy/snapshot/healthz confirmed). |
| `Proxied` is an honor-system attestation (host iptables unverifiable) | Default stays `None`; opt-in is explicit + documented; build-time proxy reachability probe; same trust model as other tiers. |
| Live path can't run in this env (no KVM) | Authored + lint-checked here; executed once on the GCP nested-virt VM via the runbook before merge. |
| Plain-HTTP egress could bypass a CONNECT-only proxy | Proxy also handles absolute-URI HTTP; netns default-deny routes *all* egress through the proxy. |
| New dev-deps trip cargo-deny | `rcgen`/`tokio-rustls` are MIT/Apache (allowlisted); verify `cargo deny check`. |
| Refactor breaks the `web` feature | `web` public API unchanged; moved tests are the regression guard; verify `cargo test -p paigasus-helikon-tools --features web`. |
```