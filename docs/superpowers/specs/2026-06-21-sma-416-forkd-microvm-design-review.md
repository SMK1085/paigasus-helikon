# Staff review — SMA-416 microVM `ExecutionBackend` (forkd) spike + skeleton

**Reviews:** [`2026-06-21-sma-416-forkd-microvm-design.md`](2026-06-21-sma-416-forkd-microvm-design.md)
**Ticket:** [SMA-416](https://linear.app/smaschek/issue/SMA-416/paigasus-helikon-tools-microvm-executionbackend-forkd-firecracker)
**Reviewed against:** Linear SMA-416, the on-disk `exec/` tree, and forkd's public docs (verified the dependency is real).
**Date:** 2026-06-21

## Verdict

Good spike discipline. The REST-not-embed decision is the right architecture, the
`guarantees()` honesty (`network: None`) is exactly the H1 trap avoided correctly, and the
`Isolation::Virtualized` addition is genuinely non-breaking (verified: the in-crate
`SandboxGuarantees` sites are struct construction, not exhaustive matches, so the new
variant compiles cleanly). I also confirmed **forkd is real and accurately described** —
Firecracker snapshot-CoW, ~100 children in ~100 ms, BRANCH ~150 ms, per-child netns +
cgroup v2 + vmgenid RNG reseed — so the spec isn't building on a phantom.

Because this is a *deliberately non-working* skeleton, the review bar is different: the
question isn't "is the code right" but "do the deferred parts hide a landmine that makes the
eventual real backend not work, or misrepresent what shipped." On that axis there are four
things worth resolving. None are blockers to the spike itself; all should be resolved before
anyone believes SMA-416 delivered a microVM tier.

## Moderate

### 1. The ticket auto-closes as "Done" while both executable ACs are deferred

Per CLAUDE.md, Linear auto-closes SMA-416 when its PR merges. But §8 re-scopes *both* halves
of the second AC — "runs a Bash command in a KVM-isolated child" **and** "non-allowlisted
egress is denied" — to skeleton/`#[ignore]`/follow-up. So the issue flips to Done in the
"Composition & Extensibility" milestone with **no Bash ever run in a VM and no egress
enforced**. The spike note + skeleton is a legitimate deliverable, but the tracker will
overstate it.

**Fix:** split SMA-416 into a spike/skeleton issue (this PR) and a live-KVM-validation +
egress-enforcement issue, or keep SMA-416 open after merge with the executable ACs tracked.
Don't let a mock-tested skeleton mark the strongest containment tier as delivered.

### 2. Snapshot provisioning — the operational dependency that makes or breaks "it runs"

The builder takes `.snapshot(template_id)` as a given, and `run()` does fork → `sh -c …` →
destroy against it. But nothing in the spec describes **who builds the warmed snapshot, what
userland it contains, or how `sh` + coreutils + the `env_allowlist` reach a usable guest**.
That snapshot *is* the difference between "skeleton compiles" and "a command actually
executes" — a fork from a snapshot without a compatible kernel/init/shell produces nothing
useful. It isn't even listed under out-of-scope.

**Fix:** the spike note should specify the snapshot contract — the guest image's expected
contents, who provisions/warms it, and how the exec step targets a real userland — even if
building it is a follow-up. Otherwise the first person to wire a live forkd has no idea what
to point `template_id` at.

### 3. TLS trust to the controller is unspecified — and decision #3 amplifies it

The builder offers `bearer_token` over "rustls + bearer" to `https://127.0.0.1:8080`, but no
knob to establish trust in the controller's certificate. rustls rejects self-signed certs by
default, so a localhost daemon needs a configured root CA / cert pin — or the implementer
falls back to `danger_accept_invalid_certs`, which silently neuters TLS. Decision #3
explicitly supports running the client **against a remote Linux KVM host**, which turns a
loopback non-issue into a real network exposure: an unvalidated cert over the network leaks
the bearer token to a MITM.

**Fix:** add a CA / cert-pinning option to the builder and document the localhost
(self-signed) and remote (real CA) trust stories. Don't let the portability win (#3) ship
without the cert story it implies.

### 4. The mock tests are green-by-construction against an *assumed* alpha API

The wiremock suite asserts the fork→exec→destroy sequence, headers, and error envelopes —
but it tests against a mock the team *built to its own assumption* of forkd's REST contract.
forkd is pre-1.0 with explicit API churn (per the ticket), and nothing executable validates
the real contract (the live path is `#[ignore]`'d). So the PR can land all-green with REST
assumptions that have never touched a real controller.

**Fix:** pin the mock to forkd's *documented* API (OpenAPI/schema if it has one), and capture
a transcript from a real controller even without `/dev/kvm` — the daemon almost certainly
starts and returns real error envelopes for fork requests, which reveals the actual shapes.
Treat the mock as "matches forkd's published contract," not "matches our guess."

## Minor

### 5. "Strongest tier" framing collides with `network: None`

The skeleton reports `network: Isolation::None` — the same as `HostBackend` and **weaker than
`OsSandboxBackend`'s `OsKernel`** on the egress axis — while the prose sells microVM as "the
strongest containment level." Per-axis honesty is right, but a user comparing `guarantees()`
sees the "strongest" tier losing on network. The mdBook "led by where it sits in the
containment ladder" framing fights this. State plainly that the skeleton microVM is **not
network-contained today and is weaker than OsSandbox on egress** until the proxy follow-up.

### 6. CoW means the warmed snapshot's state is shared across every fork

forkd reseeds `/dev/urandom` per child (vmgenid), so RNG reuse is *not* a concern — good. But
filesystem/memory state in the warmed parent is inherited CoW by all children, so any
credential or secret baked into the snapshot is visible to every sandboxed run. Add a
one-line deployment note: don't provision the warmed snapshot with the agent's secrets
present.

### 7. `syscalls: Virtualized` overloads the axis

A microVM doesn't *filter* syscalls — the guest makes any syscall to its own kernel; isolation
is the VM boundary. Reporting `syscalls: Virtualized` is defensible but means something
categorically different from `OsKernel` (seccomp filtering a host syscall set). This is the
same per-axis-semantics drift flagged for Seatbelt; for a VM, `filesystem` and `syscalls`
arguably collapse into "the whole machine is virtualized." Worth a doc sentence so consumers
don't read `Virtualized` syscalls as "restricted."

### 8. Output cap / timeout is re-implemented, not "shared"

The forkd backend is a REST client, so it can't use `spawn_capped` (pipes / process-group
kill). The `max_output_bytes` cap and the timeout-then-VM-teardown are new code against the
HTTP response — "shared … cap" overstates reuse. Make sure the VM-teardown-on-timeout path
has its own test (the mock can simulate a slow/oversized body).

### 9. Bearer-token hygiene

Ensure the token never lands in `ForkdError`/`ToolError::Other` messages or trace spans
(reqwest errors carry the URL; keep the auth header out). SMA-414's output redaction won't
catch a construction-time error.

### 10. Sanctioned scope departure from the ticket's "Linux/KVM/x86_64 only"

Decision #3 (portable REST client, not `cfg`-gated) is a reasonable, well-argued deviation
from the ticket's embedding-era phrasing, and it's recorded in the spike note — good. Just
update the ticket AC text too, so it isn't later read as unmet (same housekeeping as the
SMA-426 `#[ignore]` divergence).

## What the spec got right

- **forkd is real and faithfully characterized** — I verified the CoW/fork-time/BRANCH/
  per-child-netns claims against its public docs; the alpha/CVE/`memory.max`/single-host risk
  list matches the ticket. The spec isn't hand-waving the dependency.
- **REST-not-embed is the correct architecture** — no KVM/VMM crates in the build (compiles on
  the macOS/CI matrix), mirrors the `web/` reqwest backends, keeps the alpha VMM swappable
  behind a process boundary, and makes a cloud `E2bBackend` a sibling. Genuinely good.
- **`guarantees().network = None` instead of a fictional proxied tier** — the H1 honesty trap,
  avoided correctly (the ladder caveat in #5 is a docs nuance, not a dishonesty).
- **`Isolation::Virtualized` is correctly non-breaking** — verified the in-crate sites are
  struct construction, and external matches already need a wildcard under `#[non_exhaustive]`.
- **Spike-first with an `#[ignore]`'d live test + explicit reason** (not silent-green), and
  **E2B "verify don't build"** / no premature `microvm/` abstraction — sound YAGNI and
  consistent with the SMA-413/426 honesty rule.
- **Release mechanics check out** — additive `feat` → 0.x patch, no core change, facade
  passthrough feature; the existing `reqwest` dep already carries `json`/`rustls`, so the
  `microvm` feature unions cleanly as claimed.

## Suggested next actions

1. Split or keep SMA-416 open so the microVM tier isn't marked Done on a skeleton (#1).
2. Write the snapshot-provisioning contract into the spike note (#2).
3. Add a TLS-trust knob to the builder and document localhost vs remote (#3).
4. Pin the mock to forkd's published API and capture a real-controller transcript (#4).
5. Add the "not network-contained yet / weaker than OsSandbox on egress" caveat to the docs (#5).

## Sources

- [forkd (github.com/deeplethe/forkd)](https://github.com/deeplethe/forkd) — verified dependency claims.
- [SMA-416](https://linear.app/smaschek/issue/SMA-416/paigasus-helikon-tools-microvm-executionbackend-forkd-firecracker) — ticket scope + ACs.
