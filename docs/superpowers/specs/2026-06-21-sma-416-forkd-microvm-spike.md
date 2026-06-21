# SMA-416 — forkd microVM spike note

**Ticket:** [SMA-416](https://linear.app/smaschek/issue/SMA-416/paigasus-helikon-tools-microvm-executionbackend-forkd-firecracker)
**Branch:** `feature/sma-416-paigasus-helikon-tools-microvm-executionbackend-forkd`
**Date:** 2026-06-21
**Design ref:** [`2026-06-21-sma-416-forkd-microvm-design.md`](2026-06-21-sma-416-forkd-microvm-design.md)

---

## 1. Viability

forkd (`github.com/deeplethe/forkd`) is confirmed live, actively maintained, and fit for
integration.

**License:** Apache-2.0. This is already in the cargo-deny allowlist (`deny.toml` permits
Apache-2.0); no `deny.toml` change is needed.

**Current release:** v0.5.2 (2026-06-08). The project describes its on-disk formats and API
shapes as potentially changing before 1.0; it is **alpha**. A Python SDK and TypeScript SDK
ship alongside the daemon; we integrate against the REST controller, not either SDK.

**Fallback:** If forkd proves non-viable in a follow-up (API breaks, project abandoned), the
**E2B** Firecracker controller (`e2b.dev`) exposes the same REST-over-HTTP pattern and is a
direct drop-in behind the `ExecutionBackend` trait seam. The skeleton codes against the
confirmed forkd endpoint shapes; the trait makes the swap one file.

---

## 2. Integration decision: REST client, not embedding

We integrate forkd as an **HTTP client** (`reqwest`, bearer + TLS), talking to the forkd
controller daemon over TCP. Embedding `forkd-vmm` crates is rejected.

**Rationale:**

- **No KVM/VMM crates in our build.** Embedding would pull in KVM-requiring deps that do
  not compile on macOS, Windows, or GitHub runners. The REST client compiles on every
  target; the daemon's KVM requirement is a runtime/deployment fact, not a compile gate.
- **Mirrors existing web/ backends.** All of `WebFetchTool`, `WebSearchTool`, and their SSRF
  harness already use reqwest. The mock testing pattern (wiremock) carries over directly.
- **Alpha VMM behind a process boundary.** The controller daemon is pre-1.0 with stated API
  churn risk. An HTTP boundary decouples our compile from forkd's internals; the REST
  surface is a far smaller and more stable contract than crate internals.
- **Cloud sibling at zero marginal cost.** The same trait shape admits an `E2bBackend` (also
  a Firecracker REST controller). Embedding would require a KVM host in the process; REST
  works across a network boundary, which is exactly how cloud Firecracker controllers work.

**Portability departure from the ticket's "Linux/KVM/x86_64 only" framing:**
The ticket assumed embedding, which would have required `#[cfg(target_os = "linux")]` gates.
The REST client is **not compile-gated**. A developer on macOS can configure a `ForkdBackend`
pointing at a remote Linux KVM host (or the E2B API) and it compiles and runs cleanly. The
daemon's KVM/Linux/x86_64 requirement surfaces at runtime if no controller is reachable. This
is a deliberate decision: runtime failures are recoverable errors; compile-time gates that
block cross-platform development are not.

---

## 3. Risk assessment

| Risk | Detail | Mitigation |
|------|--------|------------|
| Pre-1.0 API churn | forkd states on-disk formats and API shapes may change before 1.0 | REST boundary absorbs churn; update is one file. Pin a known-good version in `Cargo.toml` dev-dep / lockfile note (daemon, not crate). |
| Path-traversal CVE (daemon, 0.1.0–0.1.3) | `POST /v1/sandboxes`'s `snapshot_tag` parameter bypassed the `is_safe_tag` check; fixed in 0.1.4 (PR #54). | Require daemon ≥ 0.1.4 in docs. Our client only sends the caller-supplied `template_id`; treat it as untrusted input — document that operator must validate snapshot tags via forkd's own `validate_tag()` rules (`[A-Za-z0-9_][A-Za-z0-9._-]{0,63}`). |
| Path-traversal CVE (CLI, 0.1.0–0.1.2) | `--tag` flag used unsanitized values; fixed in 0.1.3. | CLI-only issue; no bearing on our REST integration. |
| Single-host only | forkd runs one daemon per host; no multi-node scheduling | Documented as a deployment constraint. Cloud sibling (E2B) handles distributed; the trait seam makes the swap trivial. |
| `memory.max`-only quota | Only memory (cgroup v2 `memory.max`) is enforced; no CPU, IO, or PID quotas in v0.5 | Document the resource-limit gap. Agent code that pins CPU inside the VM is not bounded; `timeout_secs` on exec is the only other cap we control from the client side. |
| Network egress: none by default | forkd documents "default-deny egress policies" as a **production readiness gap** in v0.5; the daemon applies MASQUERADE but no default-deny | The skeleton carries `EgressPolicy` config but does not enforce it. `guarantees().network` honestly reports `Isolation::None`. Enforcement is deferred to SMA-437 (netns + CONNECT proxy layers). |
| No third-party security audit | forkd lists "third-party security audit" as a production readiness gap | Treat the controller as an untrusted-input-handling boundary; bearer token is the only auth layer; validate all controller responses. |
| Bearer token exposure | Token in request headers; reqwest errors carry URL but not headers | Never surface the bearer token in `ForkdError` or `ToolError` messages or trace spans. Redact at source. |

---

## 4. Snapshot (guest image) contract

The `snapshot_tag` field the builder takes is the linchpin between "skeleton compiles" and "a
command actually executes." A fork from a snapshot with no usable userland produces nothing.
This PR does not build the guest image (that requires a Linux KVM host), but the contract is
specified here so the first operator to wire a live forkd knows what a `snapshot_tag` must
point at.

**Guest contents required:**

- A Linux guest (kernel + minimal init) booted to a state where forkd's exec API can run
  arbitrary args (`/bin/sh`, coreutils, and whatever runtimes the agent's commands need must
  be present and on `PATH`).
- A snapshot *missing* `/bin/sh` or a working init produces a zero-output 127 exit code from
  exec — no explicit error from forkd, just a failed command.

**Who provisions and warms the snapshot:**

The **operator**, out of band, via forkd's own snapshot tooling: boot a base image, reach the
warmed-ready state (Python loaded, model weights in memory, etc.), then snapshot it with
`POST /v1/snapshots`. forkd's `n: 1` fork call (our flow) forks a child copy-on-write from
that warmed parent. Helikon consumes a `snapshot_tag` string; it does not create or manage
snapshots.

**How exec reaches the userland:**

The `POST /v1/sandboxes/:id/exec` endpoint runs the specified `args` **inside the booted
guest** (not on the host). The `args` array is passed verbatim; there is no implicit shell
expansion. To run a shell command, pass `["sh", "-c", "<cmd>"]`. forkd's exec endpoint has
**no** per-call `env` field, so the environment must be baked into the warmed snapshot's boot;
the skeleton sends no env and exposes no `env_allowlist` builder option.

**Copy-on-write shared state — deployment warning:**

Every child inherits the warmed parent's filesystem and memory copy-on-write. Any credential
or secret baked into the snapshot is visible to **every** sandboxed run — all children read
the same warmed pages until they diverge. **Do not bake the agent's secrets into the snapshot
image.**

forkd reseeds `/dev/urandom` per child via vmgenid, so randomness-based key material is not
shared across forks. Only static secrets (tokens, keys written into the warmed image before
snapshot) are the concern.

Building and maintaining guest images is deferred to the live-KVM follow-up **SMA-437**.

---

## 5. Controller TLS trust

forkd defaults to plain HTTP on `127.0.0.1:8889`. It supports native TLS via `--tls-cert` /
`--tls-key` flags on daemon startup; there is no indication it ships a built-in CA. reqwest
with the `rustls-tls` feature rejects self-signed certificates by default.

**Our approach:**

The builder exposes a `.controller_ca(pem)` option that installs the given PEM as a trusted
root for the reqwest client, enabling:

- **Localhost daemon with self-signed cert:** operator provides the daemon's self-signed CA.
- **Remote daemon with a real CA:** the system trust store suffices; no `.controller_ca`
  needed if the cert chains to a known root.

We deliberately do **not** expose `danger_accept_invalid_certs`. The bearer token is the sole
authentication credential; a silently-neutered TLS connection on a non-loopback path leaks it
to a network MITM. There is no security-vs-convenience tradeoff here: unvalidated TLS on a
remote controller is simply insecure. If no CA is provided and the controller's certificate is
not trusted by the system store, `build()` or the first request fails closed with a TLS
error — the correct behavior.

Operators running on localhost with plain HTTP (the forkd default) pass `http://` as the
controller URL; in that case TLS is not in play and `.controller_ca` is ignored.

---

## 6. Egress approach

forkd v0.5 ships per-child network namespaces (`per_child_netns: true` in the spawn request)
that provide VM-level netns isolation, but **no default-deny egress filter** — traffic is
NATted out via MASQUERADE. The daemon's own roadmap lists "default-deny egress policies" as a
future feature.

The Helikon egress plan is **layered**:

1. **Per-VM netns** (`per_child_netns: true` in every fork call) — isolates the child's
   network namespace from the host and from sibling VMs; raw TCP/UDP to arbitrary hosts still
   escapes via MASQUERADE.
2. **CONNECT proxy enforcing domain policy** — a Helikon-side proxy that gates HTTP/S egress
   against the `EgressPolicy` allow/deny list (derived from the SMA-412 `host_allowed` /
   `ip_blocked` domain filter, with the same private-IP block). All traffic from the VM is
   routed through this proxy; non-allowlisted destinations are rejected.

Both layers are **deferred** to the SMA-437 / SMA-413 §11 proxy follow-up. The skeleton
carries an `EgressPolicy` config type — so the caller can declare intent and the follow-up
wires enforcement — but does not enforce it. `guarantees().network` reports `Isolation::None`
until enforcement lands.

**Note on tier ordering:** on the egress axis specifically, the skeleton microVM is *weaker*
than `OsSandboxBackend` (which reports `Isolation::OsKernel` via seccomp socket-family
filtering). Although the microVM is the stronger overall containment tier, "strongest tier"
must not be read as "strongest on every axis today." The mdBook sandbox page states this
explicitly.

---

## 7. Confirmed API contract

The skeleton codes against forkd's documented v1 REST surface (confirmed from
`docs/API.md` in the `deeplethe/forkd` repository, v0.5.2). All non-healthcheck requests
carry `Authorization: Bearer <token>`.

| Step | Method + path | Request JSON | Response JSON |
|------|---------------|--------------|---------------|
| **Fork** | `POST /v1/sandboxes` | `{"snapshot_tag":"<tag>","n":1,"per_child_netns":true,"memory_limit_mib":<N>}` | `[{"id":"sb-…","snapshot_tag":"…","guest_addr":"…","pid":…,"memory_limit_mib":…,"created_at_unix":…}]` |
| **Exec** | `POST /v1/sandboxes/:id/exec` | `{"args":["sh","-c","<cmd>"],"timeout_secs":<N>}` | `{"stdout":"…","stderr":"…","exit_code":<N>}` |
| **Destroy** | `DELETE /v1/sandboxes/:id` | — | 204 No Content |
| **Health** | `GET /healthz` | — | `{"ok":true}` (no auth required) |

**Divergence from the task's assumed contract:**

The task brief gave an assumed contract using `/v1/vms` paths with a `command` string field.
The confirmed forkd API differs:
- Resource collection is `/v1/sandboxes`, not `/v1/vms`.
- The response to the fork call is an **array** (even for `n:1`); the skeleton takes `[0]`.
- The exec endpoint takes `args: string[]`, not a `command: string`; the skeleton wraps the
  caller's shell command as `["sh", "-c", "<cmd>"]`.
- The exec endpoint takes a daemon-side `timeout_secs` field; the skeleton sets it from the
  builder's `.timeout()` and also enforces a client-side reqwest timeout for defense in
  depth.
- Destroy is `DELETE /v1/sandboxes/:id`, not a separate path.
- There is no explicit `env` field on the exec endpoint in the confirmed API (env is handled
  by the warmed snapshot's boot environment). The skeleton therefore exposes **no**
  `env_allowlist` builder option — environment is an operator-side snapshot concern, not a
  per-exec injection.

The wiremock mock shapes in `tests/forkd_backend.rs` follow the **confirmed** API, not the
assumed contract.
