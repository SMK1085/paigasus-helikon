# SMA-413 Design Review — `paigasus-helikon-tools`: pluggable `ExecutionBackend` for Bash

**Reviews:** [`2026-06-16-tools-execution-backend-design.md`](./2026-06-16-tools-execution-backend-design.md)
**Reviewer perspective:** staff engineering — security model honesty, the sandbox-mechanism choice, and fitness against the planned design
**Date:** 2026-06-16
**Verdict:** **Approve with changes.** This is the right answer to the SMA-328 H1 finding, and the architecture is strong: containment is made a first-class axis *separate from approval*, `guarantees()` is surfaced honestly into the model-facing `description()`, the OS backend is **fail-closed**, `rlimit`s deliberately don't upgrade a containment axis, and the breaking bump is correctly classified. The thing to settle before the plan is the **sandbox mechanism**: `birdcage` is **~2.5 years stale** (latest `0.4.0`, Oct 2023) and **incomplete by its own documentation** (FS + network only) — yet the spec advertises `syscalls: OsKernel` for it (**H1**). For a security boundary, shipping v1 on an unmaintained crate that predates modern Landlock — when the ticket itself named maintained alternatives — needs a deliberate decision, and the `guarantees()` must tell the truth about what that backend actually enforces.

## What this was checked against

- **Linear** [SMA-413](https://linear.app/smaschek/issue/SMA-413) (follow-up to SMA-328; references SMA-326 PermissionPolicy, SMA-412 shared domain policy) — note its suggested mechanism was "bubblewrap or wrapping Anthropic's open-source `sandbox-runtime`," and its network AC was domain-level egress.
- **Live ecosystem** — the `birdcage` crate's maintenance/scope (sources at the end).
- **Code (ground truth; tools `0.1.6`)** — `tools/src/{bash.rs, sandbox.rs}`, `core/src/tool.rs`, `deny.toml`, facade `Cargo.toml`.

Severity legend: **H** = high · **M** = medium · **L** = low. Each item ends with a concrete **Correction**.

---

## H — High-severity

### H1. The chosen mechanism (`birdcage`) is stale and narrower than the spec advertises — and `guarantees()` overstates it

The whole point of this ticket is *honest containment* — so the foundation must actually deliver what `guarantees()` claims, and stay maintained against an evolving kernel threat model. Verified facts on `birdcage`:

- **Unmaintained signal:** latest release is **`0.4.0`, dated 2026… no — October 2023** — ~2.5 years before this spec. Landlock has shipped multiple ABIs since (network/TCP restrictions arrived in ABI v4 / kernel 6.7, *after* birdcage 0.4), so a 2023 crate predates the kernel features a 2026 "os-sandbox" would want, and won't carry fixes for escapes found since.
- **Narrower than advertised:** Phylum's own description is that birdcage "focuses **only** on Filesystem and Network operations and is **not a complete sandbox** preventing all side-effects." But §8 / the `SandboxGuarantees` table claim **`syscalls: OsKernel`** ("birdcage's default seccomp filter"). birdcage uses seccomp as an internal *mechanism* (e.g. to block network on older kernels); it does **not** expose a general syscall-restriction *guarantee* (ptrace, kernel-attack surface, etc. are not its remit). Advertising `syscalls: OsKernel` is exactly the kind of overselling SMA-328 H1 was about — reappearing in its own fix.
- **The ticket named maintained alternatives:** SMA-413's scope text proposed "bubblewrap or wrapping Anthropic's open-source `sandbox-runtime`." The spec rejected those for birdcage (no external binary). Reasonable on ergonomics — but trading a *maintained, agent-purpose* sandbox for an unmaintained library on a *security* axis is the wrong default unless birdcage clears a maintenance bar.

The spec hedges well — "verify API/license/maintenance in planning" and "the trait seam lets us swap mechanism with no `BashTool` change." That hedge is the right architecture. But the *shipped v1 backend choice* still matters, and the planning "verify maintenance" step should be a **gate**, not a footnote.

**Correction.**
- Make the maintenance check a go/no-go: if `birdcage` is effectively abandoned, do **not** ship it as the security backend. Re-evaluate against the ticket's own suggestions — Anthropic's `sandbox-runtime`, `bubblewrap` (`bwrap`), or building directly on the **maintained** `landlock` + `seccompiler` crates (the spec rejected hand-rolling for complexity, but those primitives are current and the trait seam contains the complexity to one file).
- **Fix `guarantees()` to reflect reality**: for the birdcage backend, report `filesystem: OsKernel`, `network: OsKernel-when-denied`, and **`syscalls: None`** (or a separately-honest value) unless the backend genuinely installs a syscall-restriction policy. The credibility of the whole `guarantees()` design depends on it being accurate for the backend you ship.
- **Confirm the license** (the spec flags it): Phylum's CLI is GPL-3.0; if `birdcage` is copyleft it would both fail the `deny` gate and impose copyleft on the *published* `paigasus-helikon-tools` crate. Verify before committing.

---

## M — Medium

### M1. `pre_exec` fork-safety: applying birdcage + `setrlimit` in the child needs verification

The current `bash.rs` uses the `Command::process_group(0)` builder method, **not** raw `pre_exec` — so this PR introduces the `unsafe` `pre_exec` pattern for the first time. Code in `pre_exec` runs in the forked child *before* `exec` and must be **async-signal-safe** (no allocation, no locks, no non-reentrant calls). `setrlimit` is fine. But `birdcage`'s normal model is to lock the **current** process and then `exec` — not to be applied inside a spawned child's `pre_exec`. If `birdcage::lock()` allocates/logs/takes locks, calling it from `pre_exec` is unsound. This shapes the architecture: if birdcage can't be applied fork-safely, the design needs a re-exec helper (the tools binary re-execing itself to self-sandbox then exec the shell), which changes §6/§8.

**Correction.** Confirm the birdcage (or replacement) application model — `pre_exec` vs same-process-lock-then-exec vs a self-re-exec helper — *before* the plan, since it dictates the `spawn_capped` shape. Document the chosen model and why it's fork-safe.

### M2. `RLIMIT_AS` default-on is a blunt memory cap that breaks legitimate commands

§7 sets `RLIMIT_AS` ("the memory cap") on by default. `RLIMIT_AS` limits *virtual address space*, which modern allocators and multithreaded programs over-reserve (per-thread stack reservations, large `mmap` of files that are never fully resident), so a default `RLIMIT_AS` will spuriously kill legitimate shell commands (compilers, anything threaded, anything mmap-ing a large file). For a tool that runs *arbitrary* commands, that's a confusing footgun.

**Correction.** Either make `RLIMIT_AS` opt-in (keep `RLIMIT_CPU`/`RLIMIT_FSIZE` on by default), set a generous default, or prefer `RLIMIT_DATA` / cgroup memory accounting for the "memory" cap. Whatever the choice, document that the address-space cap is approximate and can reject valid programs.

---

## L — Low

### L1. The default backend is still unconfined — steer users to `OsSandboxBackend`

`HostBackend` remains the default and is honestly labeled "host (no containment)." The breaking reshape (`BashTool::builder(backend)`) does force the user to *pick* a backend (good — no silent unsafe default), but `HostBackend` is the always-works choice, so the path of least resistance is still unconfined. The honest `guarantees()`/`description()` mitigate this. Make sure the docs/mdBook and the example **lead with** `OsSandboxBackend` on supported platforms (with the fail-closed fallback story), rather than presenting Host as the obvious pick.

### L2. Kernel-feature dependencies + the re-scoped network AC

`OsSandboxBackend`'s FS enforcement needs Landlock (kernel ≥ 5.13) and its network-deny likely needs unprivileged user namespaces (disabled on some hardened hosts / inside some containers). Fail-closed correctly turns those into `build()` errors, but the deployment reality is: on a host without userns, the user gets *either* "no sandbox (Err)" *or* an explicit fall back to unconfined Host. Document the kernel/feature matrix. Also note the disclosed scope cut: the ticket AC "outbound to a non-allowlisted *domain* is denied" is re-scoped to the proxy follow-up (§11); this PR's network is binary deny/allow — reconcile the ticket AC so it isn't read as delivered here.

---

## Verified OK (checked against source + ecosystem) — this is a strong response to SMA-328 H1

- **The conceptual model is exactly right** — "containment ≠ approval ≠ resource-capping," with `ExecutionBackend` as a swappable axis, DI'd into a slimmed `BashTool`. This is precisely the fix the SMA-328 review asked for (real OS containment as an option) plus the honesty layer.
- **`guarantees()` surfaced into the model-facing `description()` + traces** directly closes the SMA-328 "sandboxed Bash isn't sandboxed, and the model isn't told" gap — the model is told which tier is live. `HostBackend` stays labeled "host (no containment)" on **every** platform. (Just make the birdcage label/axes accurate — H1.)
- **Fail-closed `OsSandboxBackend::build()`** (Err rather than silent downgrade) is the correct, security-conscious default and the antithesis of the SMA-328 over-promise.
- **`rlimit`s deliberately do NOT upgrade a containment axis** ("resource hygiene, not access containment") — keeps the taxonomy honest; correct call.
- **Bump classification is correct** — a breaking `BashTool` reshape on a `0.x` crate → **minor** `0.1.6 → 0.2.0`, flagged via `feat(tools)!:` / `BREAKING CHANGE:` so release-plz selects minor. This is the *right* handling (a welcome contrast with the patch-vs-minor slips flagged in SMA-325/326/412), and "no core change ⇒ no ascend, no facade bump" is accurate.
- **CI honesty principle is excellent** — where a runner can't enforce Landlock/seccomp/Seatbelt, the test is `#[ignore]`'d with an explicit reason, **never silently skipped to green**. For a security feature this is the correct stance (a sandbox test that passes because the sandbox is inactive is worse than no test). Verify GH runner kernels (Landlock + userns) in planning, as the spec says.
- **The trait seam is the right hedge** — it makes H1's mechanism concern recoverable (swap birdcage with zero `BashTool` change), and reserves `Isolation::Virtualized`/`Proxied` (`#[non_exhaustive]`) for SMA-416 microVM + the egress proxy.
- **Integration facts check out** — `BashTool` today holds `Sandbox` + config and the `sh -c`/`process_group(0)`/`kill(-pgid)`/output-cap machinery the spec moves into `spawn_capped`; `ToolError` (`InvalidArgs`/`Denied`/`Other`, `#[non_exhaustive]`) needs no new variant; the `web` feature is the right precedent for `os-sandbox`; the facade `tools-web` forwarding pattern maps to `tools-os-sandbox`; `deny.toml` already covers the likely-permissive transitive licenses (modulo H1's birdcage-license check).

---

## Required before writing the plan

1. **H1** — gate the mechanism on a real maintenance/scope/license check: if `birdcage` is abandoned (0.4.0 / 2023) or copyleft, choose a maintained path (Anthropic `sandbox-runtime`, `bwrap`, or `landlock` + `seccompiler` directly), and **make `guarantees()` accurate** for whatever ships (don't claim `syscalls: OsKernel` unless it's true). The trait seam makes this swap cheap — use it.

Recommended alongside: **M1** (settle the `pre_exec`/fork-safe application model — it shapes the architecture), **M2** (don't default-enable `RLIMIT_AS`). L-items are docs/scope reconciliation.

## Sources

- Linear [SMA-413](https://linear.app/smaschek/issue/SMA-413) · [SMA-328](https://linear.app/smaschek/issue/SMA-328) (the soft-confinement this hardens) · [SMA-412](https://linear.app/smaschek/issue/SMA-412) (shared domain policy for the egress follow-up) · [SMA-326](https://linear.app/smaschek/issue/SMA-326) (PermissionPolicy = approval)
- `birdcage` (latest `0.4.0`, Oct 2023; "not a complete sandbox", FS + network only): https://crates.io/crates/birdcage · https://github.com/phylum-dev/birdcage
- Maintained primitives: `landlock` https://crates.io/crates/landlock · Rust sandboxing background https://oneuptime.com/blog/post/2026-01-07-rust-sandboxing-seccomp-landlock/view
- Repo: `crates/paigasus-helikon-tools/src/{bash.rs, sandbox.rs}`, `crates/paigasus-helikon-core/src/tool.rs`, `deny.toml`
- Related reviews: SMA-328 (the "sandboxed Bash isn't sandboxed" H1 this answers), SMA-412 (SSRF/domain policy, cargo-deny TLS already passing), SMA-326 (containment-vs-approval)
