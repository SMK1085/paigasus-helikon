# SMA-437 — `paigasus-helikon-tools`: forkd microVM — egress enforcement + live-KVM validation

**Status:** revised after adversarial challenge — pending GATE 1 approval
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
end-to-end on KVM**. It delivers, in one PR (structured as separable commits):

1. **Egress enforcement (the CI-provable code).** Promote SMA-412's host/IP/SSRF
   policy to a **public shared type** (`net::EgressPolicy`), build a **CONNECT
   egress proxy** (`net::EgressProxy`) that enforces the domain allow/deny policy +
   the private-IP (SSRF) block, add the `Isolation::Proxied` variant, and upgrade
   `ForkdBackend::guarantees().network` from `None` to `Proxied` when the backend is
   built in enforced-egress mode. **All pure async Rust, unit/loopback tested in CI —
   no KVM required.**

2. **Live-KVM validation (operator-run code + procedure).** A **cloud-agnostic,
   Dockerized forkd+KVM harness** (`docker compose` running forkd with `--device
   /dev/kvm` + the egress proxy sidecar + a **committed, reviewable per-VM netns
   default-deny + REDIRECT iptables ruleset**), a **guest-image build script** (with a
   secret-scan gate), a **GCP nested-virtualization reference launch script**
   (AWS/Hetzner/DO documented as alternatives), an **env-gated live integration
   test**, and a **runbook**. Run on a real x86_64 KVM host (a GCP nested-virt VM),
   with the live output attached to the PR.

**Hard environment constraint (the reason for the two-part split).** The dev host is
macOS on Apple Silicon (arm64) with no nested virtualization, and GitHub-hosted CI
has no `/dev/kvm`. Verified empirically: a container here has **no `/dev/kvm` and no
`vmx`/`svm` flag**, and forkd ships **x86_64-Linux-only** (v0.5.2) with **no native
Docker image**. So KVM cannot run here at all — everything KVM-dependent is authored +
lint-validated here and **executed on an x86_64 GCP nested-virt VM** (credentials via
interactive `gcloud auth login`, never pasted into chat).

`-tools` is at `0.2.6`; additive change (new `#[non_exhaustive]` enum variant + new
public types in a feature-gated module, **no source-breaking renames**) → patch bump
**`0.2.7`**, normal release-plz flow, **no `paigasus-helikon-core` change**.

## 2. Decisions resolved in brainstorming

1. **Maximal scope, one PR (Sven's call).** All four ACs' code/scripts/docs land
   together, structured as separable commits (policy+proxy+variant+TLS first; harness/
   scripts/runbook+live test second) so the security-critical proxy gets a clean
   review. (The challenger recommended splitting into two PRs; surfaced at GATE 1.)
2. **Live validation = a Dockerized forkd+KVM harness on GCP nested-virt**, exercised
   via an env-gated live test + a runbook, with output attached to the PR. **No
   self-hosted GitHub Actions workflow.** The harness is cloud-agnostic; GCP is the
   reference provisioner.
3. **Egress enforcement is layered** (SMA-416 §6 / SMA-413 §11). **Layer 1 — per-VM
   netns default-deny** (host iptables in each child netns: DROP all egress except DNS
   (UDP+TCP) to a vetted resolver and TCP to the egress proxy port; no iptables REDIRECT
   — non-proxy-aware traffic is simply dropped; everything else — raw TCP, UDP/QUIC —
   dropped). **Layer 2 — a CONNECT/HTTPS proxy enforcing the domain policy.** Layer 1
   is the **load-bearing general mechanism** (it is what actually stops non-proxy-aware
   clients, DNS exfil, QUIC, raw TCP); Layer 2 enforces *domain* policy on the HTTPS
   that proxy-aware clients route through it. Layer 1 ships as a **committed, reviewable
   iptables ruleset** + the harness that loads it; it is live-proven (not CI-proven, no
   KVM here). Layer 2 is *our* Rust code and **is** CI-tested on loopback.
4. **`guarantees().network` honesty (the SMA-413 H1 rule), hardened per the
   challenge.** See §3.4. The upgrade to `Proxied` is gated on an explicit
   enforced-egress opt-in that *attests both layers are deployed*, with a build-time
   proxy-reachability probe, and a doc that **enumerates the bypass surface** (what
   escapes if Layer 1 is absent). Default stays `Isolation::None`. (Alternative
   considered: never raise the tier, expose policy via an advisory accessor only —
   surfaced at GATE 1.)
5. **Promotion lives in `-tools`, not `core`.** No core change → no ascend ritual.
6. **Purpose-built CONNECT proxy**, not Squid/tinyproxy — only it can enforce our
   exact `EgressPolicy` semantics + reuse `ip_blocked` for the rebinding guard.

## 3. Egress enforcement (the CI-provable code)

### 3.1 Promote the SMA-412 policy to a public shared type — **no source-breaking change**

Today SMA-412's `host_allowed` / `ip_blocked` / `GuardedResolver` / `ssrf_check` /
`build_client` are `pub(crate)` in `src/web/http.rs`, and `forkd.rs` carries a
**duplicate** domain matcher (`EgressPolicy::is_allowed`). Unify behind one public
type in a new module `src/net/`, compiled under `#[cfg(any(feature = "web", feature =
"microvm"))]`.

**Public-surface audit of the existing `EgressPolicy` (`exec/forkd.rs`, re-exported at
`lib.rs`) — every shipped item is preserved:**

| Existing `pub` item | After promotion |
|---|---|
| `EgressPolicy::deny_all()` | unchanged |
| `EgressPolicy::allow_all()` | unchanged |
| `EgressPolicy::allow_domains(..)` | unchanged |
| `EgressPolicy::deny_domains(..)` | unchanged |
| `EgressPolicy::is_allowed(&str)` | **retained as `#[deprecated(note="renamed to is_host_allowed")]` alias** delegating to `is_host_allowed` — **no source breakage** |
| (new) `is_host_allowed(&str)` | added |
| (new) `allow_private_ips(bool)` / `is_ip_allowed(IpAddr)` | added |

`EgressPolicy` gains `#[derive(PartialEq, Eq)]` (cheap; lets tests + the enforcement
config compare). The canonical path `paigasus_helikon_tools::EgressPolicy` stays — the
re-export moves from `exec` to `net`, which is not a source change for consumers.

**Empty-allow-list semantics — pinned to avoid the web/forkd drift the challenge
flagged.** The type distinguishes `allow: None` (**no restriction**) from `allow:
Some(vec![])` (**deny all**). The two consumers keep their *opposite* defaults:

- **web** (`WebFetchTool`/`WebSearchTool`): builder maps an **empty** `allow_domains`
  to `allow: None` (no restriction) — exactly today's `fetch.rs` guard. Default
  `allow_private_ips = false`.
- **forkd** `EgressPolicy::deny_all()`: `allow: Some(vec![])` (deny all) — today's
  default for the backend.

A **regression test** asserts `WebFetchTool` built with an empty `allow_domains` still
permits arbitrary hosts (the empty-list-≠-deny-all guarantee).

**`web` refactor (public API unchanged).** `web/http.rs` shrinks to re-exports from
`net`; `web/{fetch,search}.rs` and `web/backends/*` import from `net`. The moved unit
tests are the behavioral regression guard. `GuardedResolver` + `ssrf_check` +
`build_client` move to `net`; `GuardedResolver` becomes `pub` (reusable by the cloud
`E2bBackend` sibling).

### 3.2 The CONNECT egress proxy

**New module `src/net/proxy`** (under `#[cfg(feature = "microvm")]`):

- **`pub struct EgressProxy`** from an `EgressPolicy`. `pub async fn serve(self,
  listener: tokio::net::TcpListener) -> io::Result<()>`. Per connection:
  - **`CONNECT host:port` (HTTPS tunneling, primary path):** parse request line +
    headers (robustly — bounded read to `\r\n\r\n`, reject malformed/oversized);
    `policy.is_host_allowed(host)`; resolve + check **every** address with
    `policy.is_ip_allowed(ip)` (closes the DNS-rebinding window, pinning the tunnel);
    on pass → `200 Connection Established` + `copy_bidirectional`; on fail → **fast**
    `403 Forbidden` + close (no upstream connection for a denied host).
  - **Absolute-URI plain HTTP** (`GET http://host/… HTTP/1.1`): enforce `Host` against
    the policy; non-allowlisted hosts receive `403 Forbidden`; allowlisted hosts receive
    `501 Not Implemented` (plain-HTTP forwarding is not supported — `CONNECT`/HTTPS is
    the enforced path). Plain HTTP is not forwarded upstream.
- **Secret hygiene:** logs hostnames + allow/deny verdicts only — never bodies or
  `Authorization` headers.

**No transparent-REDIRECT.** Layer 1 uses DROP-default with explicit allow rules for
DNS and the proxy port only; there is no iptables REDIRECT. Non-proxy-aware traffic
is dropped at L3/L4. Proxy-aware clients (`HTTP_PROXY`/`HTTPS_PROXY`) use the
CONNECT form to reach the proxy, which enforces the domain policy.

### 3.3 The `Isolation::Proxied` variant

`Isolation` is `#[non_exhaustive]`; adding a variant is additive/non-breaking. Doc
string (honest about the bypass surface):

> *"Egress is filtered by an allow/deny **domain** policy at a CONNECT/HTTP proxy
> (application layer). `Proxied` is meaningful **only in the layered deployment**: a
> per-VM netns default-deny that drops all egress except the proxy path (and DNS to a
> vetted resolver). Without that L3/L4 default-deny, non-proxy-aware clients, DNS
> (UDP/53), QUIC/HTTP-3 (UDP/443), and raw TCP **escape** — the proxy never sees them.
> The backend cannot verify the host's netns rules, so this tier reflects an operator
> attestation (see `ForkdBackendBuilder::enforce_egress`), the same trust model the
> other tiers apply to the kernel/hypervisor. Read as 'HTTP/S egress is domain-
> filtered, given the netns default-deny,' not 'all packets are blocked.'"*

### 3.4 `ForkdBackend` integration & the hardened honesty model

- New builder opt-in: **`.enforce_egress(proxy_endpoint: impl Into<String>)`**. It
  marks the backend's **already-carried** `EgressPolicy` (set via `.egress_policy()`)
  as enforced and records where the proxy lives. There is **one** policy value — no
  dual-policy "must match" check (the challenge's ambiguity removed).
- When set:
  - `build()` runs a **proxy-reachability probe** (cheap liveness check against
    `proxy_endpoint`); an unreachable proxy **fails closed** at construction, so a
    mistyped endpoint never silently yields an un-enforced backend reporting `Proxied`.
  - `guarantees().network` reports **`Isolation::Proxied`**.
- When **not** set: `guarantees().network` stays `Isolation::None` (today's behavior).
- **Honesty caveat (on the method doc):** reachability ≠ data-path proof; the backend
  cannot verify Layer 1. `.enforce_egress()` *attests* the operator deployed both
  layers (the harness/runbook do exactly that). The bypass surface is enumerated on
  `Isolation::Proxied` (§3.3). The existing `egress_policy()` accessor remains the
  advisory read of declared intent.

**GATE 1 decision point.** The challenger argued for never raising the tier on
attestation (keep `None`, expose policy via an accessor only) as maximally honest. The
ticket AC explicitly asks to "upgrade `guarantees().network` … to `Proxied`/
`Virtualized`," so the spec's default is the hardened-attestation `Proxied`. Sven
chooses at GATE 1.

## 4. Live-KVM validation (operator-run code + procedure)

Authored + lint-validated here; **executed on an x86_64 GCP nested-virt VM**, output
attached to the PR.

### 4.1 Dockerized forkd+KVM harness — `docker/forkd/`

- **`Dockerfile`** — Ubuntu 22.04; installs forkd `v0.5.2-x86_64-linux` + a Firecracker
  binary; builds the egress proxy (`examples/egress_proxy.rs`, a thin
  `EgressProxy::serve` runner); stages the TLS cert/key + token-file paths.
- **`docker-compose.yml`** — runs with `--device /dev/kvm`, `--cap-add=NET_ADMIN` (+
  the device-cgroup rule for `/dev/kvm`; `--privileged` only if a documented minimal
  cap set proves insufficient), publishes the controller TLS port, runs the proxy
  sidecar. `forkd doctor` runs at start to fail fast on missing KVM/cgroup-v2/Firecracker.
- **`netns-deny.rules`** (committed, reviewable) — the Layer 1 iptables ruleset: per
  child netns, DROP all egress except DNS (UDP+TCP) to the vetted resolver and TCP
  to the proxy port. Non-proxy-aware traffic is dropped (no iptables REDIRECT is used
  — non-proxy-aware clients cannot reach the proxy and their traffic is simply
  dropped by the default-deny OUTPUT policy). **`entrypoint.sh`** loads
  `netns-deny.rules` and `netns-deny6.rules` (IPv6 companion), asserts the OUTPUT
  policy is DROP (fails the container start otherwise), and starts the controller.

### 4.2 Guest-image build — `scripts/forkd/build-guest-image.sh`

Builds a minimal guest rootfs (kernel + init + `/bin/sh` + coreutils via busybox), with
`HTTP_PROXY`/`HTTPS_PROXY` baked into the guest profile (a **convenience** for
proxy-aware clients; the *general* closure is Layer 1 default-deny — non-proxy-aware
traffic is dropped at the netns level, see §4.1), then warms + snapshots via
`POST /v1/snapshots` (`{tag, kernel, rootfs, rw, tap, boot_wait_secs}`, confirmed
contract). **Secret-scan gate:** before snapshotting, the script greps the rootfs for
the bearer token + common secret patterns and **fails** if any are found (the
CoW-shared-state hazard from SMA-416 §3.4 — every child inherits the warmed parent).

### 4.3 GCP nested-virt launch — `scripts/forkd/gcp-launch.sh` + `gcp-teardown.sh`

`gcloud compute instances create … --enable-nested-virtualization` on a small x86_64
Intel instance (e.g. `n2-standard-4`, zone `europe-west1-b`); startup script installs
Docker + brings the harness up. The runbook (§4.5) covers **container-level KVM
passthrough** (`--device /dev/kvm` + device-cgroup), not just host nested-virt. Teardown
deletes the instance.

### 4.4 Env-gated live tests — `tests/forkd_live.rs` (**un-`#[ignore]`'d**)

Under `#![cfg(feature = "microvm")]` (so it **compiles on every PR** under
`--features microvm` / `--all-features` — no bit-rot). **No `#[ignore]`, no separate
feature.** Each test begins with a runtime guard:

```rust
let Ok(url) = std::env::var("FORKD_URL") else {
    eprintln!("SKIP live forkd test: set FORKD_URL/FORKD_TOKEN/FORKD_SNAPSHOT to run"); return;
};
```

so in CI (no controller) it **skips loudly** (visible, not silent-green), and on the KVM
host it runs for real. Two tests:

1. **`live_forkd_runs_bash_in_a_microvm`** — `echo from-a-microvm` → asserts stdout +
   `exit_code == 0` (the promoted ex-`#[ignore]` test).
2. **`live_forkd_denies_nonallowlisted_egress`** — policy allows only `example.com`; a
   **proxy-aware** guest command hitting a **non-allowlisted** domain must fail
   **fast** (proxy 403 → curl non-zero exit, asserted *well under* the wall-clock
   timeout, so **deny is distinguished from a hang** — the challenger's Q1); the
   allowlisted domain succeeds.

The honest CI story (mirroring SMA-416 §8): CI compile-checks these; the real run is
done once on GCP with output pasted into the PR. AC#2 is re-scoped accordingly (§9).

### 4.5 Runbook — `docs/runbooks/forkd-live-validation.md`

`gcloud auth login` → `gcp-launch.sh` → `build-guest-image.sh` → `docker compose up`
(incl. container `/dev/kvm` passthrough) → run the live tests with `FORKD_*` env →
observe egress-deny → `gcp-teardown.sh`. Plus the real-CA non-loopback TLS story and
AWS/Hetzner/DO alternatives.

## 5. Controller TLS trust (end-to-end)

The builder already takes `.controller_ca(pem)` and rejects remote plain-`http`. Add a
loopback TLS integration test (`tests/forkd_tls.rs`, no KVM): **generate a fresh
self-signed cert in-test via `rcgen`** (never installed system-wide, to avoid the
platform-verifier fragility the challenge flagged), start a minimal `tokio-rustls`
server, then assert (a) a `ForkdBackend` **without** `.controller_ca` fails to connect
(assert on **connection failure**, not an error-string match), and (b) **with**
`.controller_ca(<that cert>)` it succeeds. Proves trust is enforced and
`danger_accept_invalid_certs` is never used. The runbook documents the real-CA
deployment (forkd `--tls-cert/--tls-key`; or pin the self-signed CA via
`.controller_ca`).

## 6. Tests

| Test (file) | Gate | Proves |
|---|---|---|
| `egress_proxy.rs` — deny non-allowlisted domain → fast 403 (no upstream) | CI (`microvm`) | proxy denies non-allowlisted egress |
| `egress_proxy.rs` — domain-allowed but resolves to private IP → denied | CI (`microvm`) | rebinding/SSRF guard |
| `egress_proxy.rs` — allow_private_ips + allowlisted loopback → bytes tunnel | CI (`microvm`) | the allow path proxies |
| `net::policy` unit tests (moved from `web/http.rs`) | CI | promoted policy unchanged |
| `web` empty-`allow_domains` → all hosts permitted | CI (`web`) | no empty-list deny-all regression |
| `forkd_backend.rs` mock fork→exec→destroy, timeout/teardown, truncation, 5xx | CI (`microvm`) | skeleton contract (unchanged) |
| `is_allowed` deprecated alias still compiles + behaves | CI (`microvm`) | no source breakage |
| `guarantees()` — `Proxied` with `.enforce_egress`, `None` without | CI (`microvm`) | honesty model |
| `forkd_tls.rs` — fail without CA / succeed with CA | CI (`microvm`) | TLS trust; no danger flag |
| `forkd_live.rs` — live run + live fast-deny | `microvm`, env-gated, KVM host | AC#2 + live AC#3 |

cargo-deny verified by **running** `cargo deny check` against the resolved graph (not
asserting licenses).

## 7. Module layout

```
crates/paigasus-helikon-tools/src/
  net/                       # NEW — cfg(any(feature="web", feature="microvm"))
    mod.rs                   # re-exports
    policy.rs                # EgressPolicy (shared; is_allowed deprecated alias), ip_blocked, GuardedResolver, ssrf_check, build_client
    proxy.rs                 # EgressProxy — cfg(feature="microvm")
  web/http.rs                # shrinks to thin re-exports from net
  web/{fetch,search}.rs, web/backends/*   # import from net
  exec/mod.rs                # + Isolation::Proxied
  exec/forkd.rs              # EgressPolicy from net; .enforce_egress + probe + Proxied guarantee
  lib.rs                     # pub use net::{EgressPolicy, EgressProxy, GuardedResolver, ...}
  examples/egress_proxy.rs   # NEW — EgressProxy::serve runner used by the harness
docker/forkd/                # NEW — Dockerfile, docker-compose.yml, entrypoint.sh, netns-deny.rules
scripts/forkd/               # NEW — build-guest-image.sh (+secret-scan), gcp-launch.sh, gcp-teardown.sh
docs/runbooks/forkd-live-validation.md   # NEW runbook
crates/paigasus-helikon-tools/tests/
  egress_proxy.rs, forkd_tls.rs          # NEW (CI)
  forkd_live.rs                          # NEW — cfg(feature="microvm"), env-gated, NOT #[ignore]'d
  forkd_backend.rs                       # existing; the #[ignore]'d test moves to forkd_live.rs
```

## 8. Release & docs

- **Version:** `-tools` `0.2.6 → 0.2.7` (additive patch on 0.x). **No core change** →
  no ascend ritual. Facade cascades via release-plz `dependencies_update` (release-plz
  performs the `-tools` bump, so the cascade runs); **verify at release time** the
  facade self-pin to `paigasus-helikon-tools` moves `0.2.6 → 0.2.7`.
- **Features:** `microvm = ["dep:reqwest", "dep:url", "tokio/net", "tokio/io-util"]`
  (proxy needs `tokio/net` + `io-util` for `copy_bidirectional`; policy needs `url` +
  `tokio/net`). **No `microvm-live` feature** (live tests are env-gated, not
  feature-gated). No new facade passthrough feature.
- **Dev-deps:** `rcgen`, `tokio-rustls` (TLS test) added to `[workspace.dependencies]`
  + `[dev-dependencies]`. Licenses **verified by running `cargo deny check`** (not
  asserted): note `rcgen` pulls `yasna` (BSD-3-Clause — allowlisted) + `pem` (MIT);
  `tokio-rustls` is already in the graph via reqwest.
- **Lib-only build verification** (the reqwest-feature-gating/devdep-masking footgun):
  confirm `cargo build -p paigasus-helikon-tools --features microvm` (no `web`, no
  dev-deps) compiles the proxy + policy.
- **Docs (same PR, per CLAUDE.md):** mdBook `concepts/tools.md` microVM tier — egress
  **Proxied** when the layered enforcement is deployed (with the bypass caveat), `None`
  in the un-enforced default; update the containment-ladder note. `-tools` README
  (egress enforcement + harness/runbook pointer). Facade + root README feature→module
  notes for `Isolation::Proxied` + `EgressProxy`. Runbook under `docs/runbooks/`,
  linked from SUMMARY if it belongs in the book. `mdbook build docs/book` clean
  (`warning-policy = "error"`). `///` on every new `pub` item.

## 9. Acceptance criteria mapping (honest)

- ⚠️ **Guest snapshot image** — `build-guest-image.sh` (+ secret-scan gate) + runbook
  (contract from SMA-416 §3.4). Operator-run on the KVM host; "no secrets baked" is
  enforced by the scan step, not merely documented.
- ⚠️ **Live KVM run** — `forkd_live.rs::live_forkd_runs_bash_in_a_microvm`, un-
  `#[ignore]`'d + env-gated; **executed once on the GCP nested-virt VM, output attached
  to the PR**; CI compile-checks it (skips loudly without a controller). Re-scoped like
  SMA-416 §8 — not claimed as a CI-green gate.
- ⚠️ **Egress enforcement (layered)** — Layer 2 (`EgressProxy`) is **CI-tested**
  (non-allowlisted egress → fast 403). Layer 1 (netns default-deny + REDIRECT) ships as
  a **committed, reviewable iptables ruleset** + harness assertion; it is **live-proven**
  by `live_forkd_denies_nonallowlisted_egress` on GCP (output attached), not CI-proven.
  `guarantees().network` upgrades `None → Proxied` in enforced mode (hardened
  attestation, §3.4).
- ✅ **Controller TLS trust end-to-end** — `forkd_tls.rs` (CI, in-test cert) + runbook
  real-CA story; no `danger_accept_invalid_certs`.

## 10. Out of scope (YAGNI)

- **GC/reconciliation of orphaned sandboxes** (the `forkd.rs` decode-after-commit
  window) — the skeleton named SMA-437 for this, but minimal reconciliation (list-by-
  tag/age + reap) is a self-contained chunk; **consciously re-deferred to a new
  follow-up ticket** (filed before merge) and noted here + in the `forkd.rs` comment.
  The orphan window remains rare (decode/timeout after a committed fork).
- The `E2bBackend` cloud sibling (verified-compatible, not built).
- Unix-socket controller transport (TCP+TLS only).
- A self-hosted GitHub Actions KVM runner / required CI gate for the live path.
- Embedding `forkd-vmm` (rejected SMA-416 §2.2 seam).
- Per-call stdin/env on `ExecRequest` (forkd exec has no env field).

## 11. Risks

| Risk | Mitigation |
|---|---|
| forkd is alpha (pre-1.0 churn); pinned `v0.5.2` | REST boundary + pinned version; API re-verified against `docs/API.md`. |
| `Proxied` is an attestation (host iptables unverifiable) | Default `None`; explicit opt-in; build-time proxy probe; doc enumerates bypass surface; GATE 1 may choose to keep `None`. |
| Egress closure depends on Layer 1, not `HTTP_PROXY` | Layer 1 (netns default-deny DROP) is the load-bearing mechanism, shipped as a reviewable ruleset; `HTTP_PROXY` is convenience only; live-proven. |
| Policy unification flips web's empty-allow semantics | `allow: None` (no restriction) vs `Some(empty)` (deny-all) pinned; web maps empty→None; regression test. |
| Public-API breakage on the patch bump | `is_allowed` retained as `#[deprecated]` alias; full surface audited (§3.1). |
| Live path can't run here (no KVM) | Authored + lint-checked here; executed once on GCP via the runbook before merge. |
| Container `/dev/kvm` passthrough subtlety | Runbook covers device-cgroup + `--device /dev/kvm` (+ minimal caps), not just host nested-virt. |
| New dev-deps trip cargo-deny | `rcgen`/`tokio-rustls` licenses verified by running `cargo deny check`, not asserted. |
| Refactor breaks `web` | `web` public API unchanged; moved tests + empty-allow regression test guard it; verify `cargo test -p paigasus-helikon-tools --features web`. |
| Live test bit-rots | `cfg(feature="microvm")` (not a bespoke feature) → compiled under `--all-features` every PR; runtime env-skip keeps the CI run green without a controller. |

## 12. Challenge resolutions (2026-06-22 spec-challenger)

- **B1 (AC#3 done-by-doc):** folded — Layer 1 shipped as a committed reviewable
  ruleset + harness assertion; AC#3 mapping downgraded to ⚠️; live fast-deny test with
  attached output.
- **B2 (false no-breakage):** folded — `is_allowed` kept as `#[deprecated]` alias;
  public surface audited (§3.1).
- **B3 (`Proxied` unsound):** folded — bypass surface enumerated on the variant doc;
  two-layer attestation; build-time probe; default `None`; GATE 1 alternative noted.
- **M4 (split PRs):** not auto-folded — conflicts with Sven's single-PR + run-live-now
  choice; one PR with separable commits; surfaced at GATE 1.
- **M5 (HTTP_PROXY ≠ universal):** folded — Layer 1 default-deny DROP is load-bearing;
  HTTP_PROXY demoted to convenience; non-proxy-aware traffic is dropped at L3/L4
  (no transparent REDIRECT — DROP only).
- **M6 (un-#[ignore]):** folded — dropped the `microvm-live` feature; env-gated,
  literally un-`#[ignore]`'d, compiled every PR, loud CI skip; AC#2 re-scoped.
- **M7 (empty-allow drift):** folded — semantics pinned + regression test (§3.1).
- **M8 (orphan GC):** folded — consciously re-deferred to a named follow-up (§10).
- **MINORs/QUESTIONs:** folded — `tokio/io-util`; rcgen license verified not asserted;
  in-test cert; secret-scan; fast-deny vs timeout; container KVM passthrough; lib-only
  build check; facade-cascade verify; `EgressPolicy` gains `PartialEq`.
```