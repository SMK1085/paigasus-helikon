# SMA-416 — `paigasus-helikon-tools`: microVM `ExecutionBackend` (forkd / Firecracker) — spike + skeleton

**Status:** approved (brainstorm) — pending written-spec review
**Ticket:** [SMA-416](https://linear.app/smaschek/issue/SMA-416/paigasus-helikon-tools-microvm-executionbackend-forkd-firecracker)
**Milestone:** Composition & Extensibility
**Branch:** `feature/sma-416-paigasus-helikon-tools-microvm-executionbackend-forkd`
**Date:** 2026-06-21
**Builds on:** [SMA-413](https://linear.app/smaschek/issue/SMA-413/paigasus-helikon-tools-pluggable-executionbackend-for-bash-host-os) (the `ExecutionBackend` trait) · related: [SMA-328](https://linear.app/smaschek/issue/SMA-328) (sandbox harness), [SMA-375](https://linear.app/smaschek/issue/SMA-375) (cargo-deny)

## 1. Summary

Add a **KVM-isolated microVM** execution tier — the strongest containment level —
as a third sibling backend behind the SMA-413 `ExecutionBackend` trait, alongside
`HostBackend` (no containment) and `OsSandboxBackend` (Landlock + seccomp).

The candidate is **forkd** (`github.com/deeplethe/forkd`): a Rust, Apache-2.0
Firecracker controller that forks ~100 microVMs in ~101 ms from a warmed
copy-on-write parent snapshot. We integrate as a **REST client** of its controller
daemon — not by embedding the VMM — so our crate stays portable (compiles on
macOS/CI with **no KVM or VMM crates in our build**) and the alpha VMM stays fully
swappable behind the trait.

This ticket is explicitly **spike-first**. This PR delivers:

1. A **spike note** (§5) recording the integration decision (REST vs embed), the
   risk assessment, and the egress approach — the first AC.
2. A **compiling, mock-tested `ForkdBackend` skeleton** (§3–§4) behind a new
   `microvm` Cargo feature, with the fork→exec→destroy REST flow, output/timeout
   capping, an `EgressPolicy` config seam, and honest `guarantees()`.

What this PR deliberately does **not** do is run a real microVM end-to-end:
GitHub runners expose no `/dev/kvm`, and the development host is macOS, so the live
KVM path is `#[ignore]`'d with an explicit reason and validated manually on a Linux
KVM host as a follow-up. **Egress enforcement** (netns + CONNECT proxy) is also
deferred — the skeleton carries the policy config; the layers land with the
SMA-413 §11 proxy follow-up. This re-scopes the ticket's second AC ("non-allowlisted
egress is denied") to *carry-and-document* in this PR; see §3.3 and §8 for the
honesty rationale.

`-tools` is at `0.2.5`; this is an **additive** change (new feature-gated type + a
new `#[non_exhaustive]` enum variant) → patch bump `0.2.6`, normal release-plz flow,
**no `paigasus-helikon-core` change**.

## 2. Scope decisions

1. **Spike note + compiling skeleton — not a working KVM backend.** A real
   KVM-isolated run cannot be CI-verified (no `/dev/kvm` on GitHub runners) and
   cannot be verified on the macOS dev host at all. The honest deliverable is the
   spike note plus a feature-gated `ForkdBackend` that compiles everywhere, is
   unit-tested against a **mocked** forkd controller, and exposes a working trait
   seam. The live path is `#[ignore]`'d (§6).

2. **Integration seam: REST controller API, not embedding `forkd-vmm`.** We talk to
   the forkd controller daemon over HTTP (reqwest, bearer + rustls). Rationale:
   (a) **no KVM/VMM crates in our build** → compiles on macOS/Windows/CI; (b)
   mirrors the existing `web/` backends (reqwest-based); (c) keeps the **alpha** VMM
   behind a process boundary so its API churn never breaks our compile; (d) makes a
   cloud `E2bBackend` (also a Firecracker REST controller) a trivial sibling.
   Embedding is rejected: it pulls KVM-requiring deps that will not compile on the
   dev/CI matrix and couples our build to forkd's alpha internals.

3. **The REST client is PORTABLE — not Linux-gated.** Because the backend is just an
   HTTP client, the type compiles on every target; the daemon's KVM/Linux/x86_64
   requirement is a **runtime/deployment** fact, documented, not a `cfg` compile
   gate. This is a deliberate departure from the ticket's "Linux / KVM / x86_64
   only" phrasing, which assumed embedding. Consequence: a developer can run the
   client on this Mac against a **remote** Linux KVM host — and the same shape backs
   the cloud `E2bBackend`. Recorded as a decision in the spike note (§5).

4. **Egress: carry the policy now, enforce later (layered).** The skeleton threads an
   `EgressPolicy` (domain allow/deny) through the builder but does **not** enforce
   it. The spike note recommends the **layered** enforcement model — per-VM netns
   default-deny (raw TCP/UDP) **plus** a CONNECT proxy enforcing the promoted
   SMA-412 `host_allowed`/`ip_blocked` domain policy (HTTP/S) — both landing with
   the SMA-413 §11 proxy follow-up. forkd ships only a shared MASQUERADE with no
   default-deny egress, so enforcement is necessarily a Helikon-side layer.

5. **`guarantees()` stays honest (the SMA-413 H1 trap).** Because egress is *not*
   enforced in the skeleton, `network` is reported as `Isolation::None`, not a
   virtualized/proxied tier. `filesystem` and `syscalls` are `Isolation::Virtualized`
   (a real hypervisor boundary). The label marks the backend **experimental**. We
   never advertise containment we do not enforce.

6. **`E2bBackend`: verify, don't build.** The object-safe, portable-REST shape
   already admits a cloud Firecracker sibling. We confirm the trait does not block it
   and note it — we do **not** build a premature shared `microvm/` abstraction now
   (YAGNI). One `forkd.rs`.

## 3. The `ForkdBackend` skeleton

### 3.1 Module layout & public surface

```
crates/paigasus-helikon-tools/src/
  exec/
    mod.rs        # + Isolation::Virtualized variant; re-export forkd surface
    forkd.rs      # ForkdBackend + ForkdBackendBuilder + ForkdError + EgressPolicy
                  #   [cfg(feature = "microvm")]   (NOT target-gated — portable client)
  lib.rs          # re-exports under #[cfg(feature = "microvm")]
```

Public re-exports (under `#[cfg(feature = "microvm")]`): `ForkdBackend`,
`ForkdBackendBuilder`, `ForkdError`, `EgressPolicy`. Always-available additive
change: `Isolation::Virtualized`. Every `pub` item carries a `///` doc.

### 3.2 Builder & types

```rust
// #[cfg(feature = "microvm")]
ForkdBackend::builder(controller_url)   // e.g. "https://127.0.0.1:8080"
    .bearer_token(token)                // controller auth (rustls + bearer)
    .snapshot(template_id)              // warmed parent snapshot to fork from
    .timeout(Duration)                  // default 30s (DEFAULT_TIMEOUT)
    .max_output_bytes(1 << 20)          // default 1 MiB (DEFAULT_MAX_OUTPUT)
    .env_allowlist(["PATH", "HOME"])    // forwarded into the guest
    .egress_policy(EgressPolicy::deny_all().allow_domains(["pypi.org"]))
    .build() -> Result<Arc<dyn ExecutionBackend>, ForkdError>
```

- `EgressPolicy` — a domain allow/deny config the backend **carries**. Modeled on
  the SMA-412 web policy intent (allow domains, deny domains, deny private/link-local
  IPs); the *enforcement* engine is the proxy follow-up. Defined here as a public
  type so the follow-up can reuse it (and so the cloud sibling shares it).
- `ForkdError` — crate-local `thiserror` / `#[non_exhaustive]` enum for
  **construction** failures (malformed URL, missing token), parallel to
  `OsSandboxError`. Runtime failures surface as `ToolError::Other` from `run`.
- Transport scope: **TCP + TLS + bearer** (clean with reqwest). The forkd
  Unix-socket transport (needs a custom connector, e.g. `hyperlocal`) is a noted
  follow-up, not in this skeleton.

### 3.3 `run` flow & `guarantees()`

`run(req)` drives the controller REST API:

1. **Fork** a microVM from the warmed `snapshot` (COW) → VM id.
2. **Exec** `sh -c <req.command>` inside, forwarding the env allowlist.
3. **Collect** stdout/stderr/exit, applying the shared `max_output_bytes` cap and
   the wall-clock `timeout` (on timeout, request VM teardown; report `timed_out`).
4. **Destroy** the VM (best-effort, in a guard so it runs on the error path too).

Non-zero exit, timeout, and truncation are **normal `ExecOutput` fields**, never
errors — identical contract to the other backends. Daemon-unreachable / fork-failed
/ malformed-response → `ToolError::Other(anyhow)`.

`guarantees()` (the honesty call, §2.5):

```rust
SandboxGuarantees {
    filesystem: Isolation::Virtualized,   // separate guest kernel + rootfs
    syscalls:   Isolation::Virtualized,   // guest kernel; host syscall surface not shared
    network:    Isolation::None,          // egress NOT filtered yet (shared MASQUERADE)
    label: "forkd (firecracker microvm — experimental)",
}
```

When the layered egress policy lands, `network` upgrades to the appropriate tier
(`Proxied` / `Virtualized`) — added by that follow-up, non-breaking.

## 4. The `Isolation::Virtualized` variant

`Isolation` is `#[non_exhaustive]`; SMA-413's design explicitly reserved
`Virtualized` for this ticket. Adding it is additive/non-breaking (downstream
matches already require a wildcard arm). Doc: "Enforced by a hardware-virtualization
(KVM/hypervisor) boundary — a separate guest kernel, stronger than `OsKernel`."
No other backend changes its `guarantees()`.

## 5. Spike note (distinct committed artifact — first AC)

A standalone `docs/superpowers/specs/2026-06-21-sma-416-forkd-microvm-spike.md`,
committed on the branch, recording:

- **Spike Step 1 — viability verification.** Confirm forkd exists, is **Apache-2.0**
  (fits the cargo-deny allowlist — already permits Apache-2.0, so no `deny.toml`
  change expected; confirm with `cargo deny check`), and that its controller REST
  surface (fork / exec / destroy, bearer + rustls, Unix/TCP) matches the ticket's
  description. **Fallback:** if forkd proves non-viable, the note says so and names
  **E2B** as the alternative Firecracker controller; the skeleton is still coded
  against forkd's *documented* controller API regardless (we cannot run KVM here in
  any case), so the trait seam and tests stand either way.
- **Integration decision: REST, not embed** — with the §2.2 rationale; plus the
  §2.3 portability departure from the ticket's "Linux-only" phrasing.
- **Risk assessment** — forkd is **alpha**: pre-1.0 API churn, single-host, only a
  `memory.max` quota, a path-traversal CVE fixed in 0.1.3. Mitigation: REST process
  boundary + the trait seam keep it swappable; we pin a known-good version and treat
  the controller as untrusted-input-handling.
- **Egress approach** — the layered model (§2.4): netns default-deny + CONNECT proxy
  reusing the promoted SMA-412 domain policy; enforcement deferred to the §11 proxy
  follow-up; skeleton carries `EgressPolicy`.

## 6. Tests

- **CI (portable, always runs):** `tests/forkd_backend.rs` using **wiremock** (the
  `web/` backends' pattern) to stand up a **mock forkd controller** — asserts the
  fork→exec→destroy call sequence and headers (bearer), the `ExecOutput` mapping
  (stdout/stderr/exit/timeout/truncation), error mapping (daemon 5xx →
  `ToolError::Other`), and that the configured `EgressPolicy` is carried. A unit test
  asserts `guarantees()` reports the honest tiers (`Virtualized` fs/syscalls,
  `None` network).
- **Real daemon (`#[ignore]`'d):** one integration test exercising a live forkd +
  `/dev/kvm`, `#[ignore]`'d with an explicit reason string. **Never silently
  skipped-to-green** — per the SMA-413 honesty rule (a sandbox test that passes
  because the sandbox is inactive is worse than no test).
- **Feature gating:** all of the above behind `#[cfg(feature = "microvm")]`; the
  default build and the non-`microvm` matrix never compile reqwest for this path.

## 7. Release & docs

- **Version:** `-tools` `0.2.5 → 0.2.6`. Additive `feat(tools)` → patch on 0.x (per
  release-plz's 0.x policy). **No core change** → no ascend ritual, no manual facade
  bump; the facade auto-bumps because this PR edits its `Cargo.toml`/`lib.rs` to add
  the `tools-microvm` passthrough feature.
- **Deps:** `microvm = ["dep:reqwest", "dep:url"]` (reqwest `json` + `rustls`; unions
  cleanly with `web`'s reqwest features). No new third-party crate beyond what `web`
  already pins. `cargo deny check` stays green (no new licenses).
- **Facade:** add `tools-microvm = ["tools", "paigasus-helikon-tools/microvm"]`
  mirroring the existing `tools-web` / `tools-os-sandbox` passthrough feature lines
  (these gate the inner crate feature; they are not separate `pub use` aliases), and
  note the `microvm` requirement in the facade `tools` doc comment.
- **Docs (same PR, per CLAUDE.md):** mdBook sandbox page gains the **microVM tier**
  (clearly marked **experimental/skeleton**, KVM-only at runtime, egress deferred),
  led by where it sits in the containment ladder. Update the `-tools` README, the
  facade README, and the root README feature→module maps for `microvm` /
  `tools-microvm`. `mdbook build docs/book` stays clean
  (`warning-policy = "error"`). Crate-level + `///` docs on every new `pub` item.

## 8. Honesty & re-scoped acceptance criteria

The ticket's ACs, restated against what this PR actually delivers:

- ✅ **Spike note** recording the integration decision (REST), risk assessment, and
  egress approach — §5.
- ⚠️ **"`ForkdBackend` runs a Bash command in a KVM-isolated child returning
  stdout/stderr/exit"** — delivered as a **compiling, mock-tested** skeleton with the
  real fork→exec→destroy REST flow; the **live KVM run is `#[ignore]`'d / manual** on
  a Linux KVM host (no `/dev/kvm` in CI; macOS dev host). This is the §2.1 scope
  decision, agreed in brainstorming.
- ⚠️ **"non-allowlisted egress is denied via the layered policy"** — the skeleton
  **carries** `EgressPolicy` and the spike note **specifies** the layered enforcement
  (netns + proxy); actual enforcement is the SMA-413 §11 proxy follow-up.
  `guarantees().network` honestly reports `None` until then.
- ✅ **Trait stays swappable** for a cloud `E2bBackend` sibling — verified, not built.
- ✅ **CI green** (fmt, clippy incl. `--all-features`, the test matrix, docs,
  doc-coverage, commits, pr-title, audit, deny) and `mdbook build` clean.

## 9. Out of scope (YAGNI)

- Embedding `forkd-vmm` (the rejected seam, §2.2).
- Actual egress enforcement — the netns layer and the CONNECT proxy (SMA-413 §11).
- Unix-socket controller transport (TCP+TLS only in the skeleton, §3.2).
- A shared `microvm/` abstraction or the `E2bBackend` itself (§2.6).
- Real KVM CI / a self-hosted KVM runner.
- Per-call stdin / env overrides on `ExecRequest` (`#[non_exhaustive]` reserves room).
