# SMA-413 — `paigasus-helikon-tools`: pluggable `ExecutionBackend` for Bash

**Status:** approved (brainstorm) — revised per design review — pending written-spec re-review
**Ticket:** [SMA-413](https://linear.app/smaschek/issue/SMA-413/paigasus-helikon-tools-pluggable-executionbackend-for-bash-host-os)
**Milestone:** Composition & Extensibility
**Branch:** `feature/sma-413-paigasus-helikon-tools-pluggable-executionbackend-for-bash`
**Date:** 2026-06-16
**Design review:** [`2026-06-16-tools-execution-backend-design-review.md`](./2026-06-16-tools-execution-backend-design-review.md) — all findings incorporated (see §0).

## 0. Design-review resolutions

The first draft proposed the `birdcage` crate. The review (H1) flagged that as the
mechanism to settle before planning; verification confirmed the concern and changed
the mechanism:

- **H1 — birdcage is GPL-3.0 and FS+network-only.** Verified against crates.io /
  the repo: latest birdcage is `0.8.1` (Apr 2024; the review's "0.4.0 / Oct 2023"
  read a *yanked* version), it is licensed **GPL-3.0-or-later**, and its own README
  says it "focuses **only** on Filesystem and Network operations… is **not** a
  complete sandbox… applications can still execute most system calls." The GPL
  license is **disqualifying** for our published `Apache-2.0 OR MIT` crate (license
  conflict + `cargo deny` gate), independent of the FS-only scope. **Resolution:**
  drop birdcage; build directly on the maintained, permissive `landlock`
  (`MIT OR Apache-2.0`, 0.4.5 / May 2026) + `seccompiler`
  (`Apache-2.0 OR BSD-3-Clause`, 0.5.0 / Mar 2025) primitives. These give *genuine*
  filesystem **and** syscall enforcement, so `guarantees()` is truthful (the
  overstated `syscalls: OsKernel` is now backed by a real seccomp policy). Linux-first;
  macOS Seatbelt deferred (§11) — this matches the ticket's Linux-only AC.
- **M1 — `pre_exec` fork-safety.** The application model is settled (§8.2): the
  Landlock ruleset and the seccomp BPF program are built **in the parent** (before
  fork, where allocation is fine); the child's `pre_exec` runs **only** the
  async-signal-safe apply syscalls. No `lock()`-style allocation in the child.
- **M2 — `RLIMIT_AS` is opt-in, not default.** Address-space caps spuriously kill
  threaded/`mmap`-heavy programs; only `RLIMIT_CPU` and `RLIMIT_FSIZE` are on by
  default (§7).
- **L1 — docs lead with `OsSandboxBackend`** on Linux (§13).
- **L2 — kernel-feature matrix documented; network AC reconciled** (§8.3, §15):
  Landlock+seccomp need **no namespaces / no userns**, so the deployment matrix is
  just "Landlock ≥ 5.13 + seccomp"; this PR's network is binary deny/allow,
  domain-level egress is the follow-up.

## 1. Summary

Introduce **containment as a first-class, swappable axis, separate from approval**.
Today `BashTool` (SMA-328) is *soft confinement only* — cwd pinned, env allowlist,
timeout, output cap — and a spawned shell escapes the `cap-std` jail; the only real
control is the runner's `PermissionPolicy` / `DenyRule::tool("Bash")`, which is an
*approval* gate, not *containment*.

This ticket extracts an object-safe `ExecutionBackend` trait that `BashTool` runs
against, and ships two backends:

- **`HostBackend`** — today's behaviour, kept as the **default** and **kept
  documented as "NOT a security boundary"**, hardened with CPU/file-size `rlimit`s
  (process-group subtree-kill on timeout already shipped in SMA-328).
- **`OsSandboxBackend`** — real OS *process* containment of the filesystem and
  syscalls (**Linux**), via the permissive `landlock` (filesystem) + `seccompiler`
  (syscalls, incl. an all-or-nothing network toggle) crates. **Fail-closed.**

Swapping the backend needs **no change to tool or agent code**.

`-tools` is already released at `0.1.6`; this is a normal release-plz flow with a
**breaking** API reshape → `0.2.0`. No `paigasus-helikon-core` change is required.

## 2. Scope decisions (resolved during brainstorming + review)

1. **PR scope: trait + `HostBackend` + `OsSandboxBackend` (fs + syscalls, Linux);
   defer the network proxy and macOS.** This PR delivers the `ExecutionBackend`
   seam, DI into `BashTool`, `HostBackend` hardening, and `OsSandboxBackend` with
   OS-enforced **filesystem + syscall** containment plus an **all-or-nothing**
   network toggle, on **Linux**. Deferred to follow-ups: the deny-by-default
   **domain-level** egress proxy (needs the SMA-412 web policy promoted to a public
   type + a CONNECT proxy — §11) and **macOS Seatbelt** (no maintained permissive
   pure-Rust Seatbelt crate exists; the ticket AC is Linux-only). Consequence: the
   ticket AC "outbound to a non-allowlisted **domain** is denied" is re-scoped to
   the proxy follow-up; this PR's network story is binary (deny-all or allow-all).

2. **Enforcement mechanism: `landlock` + `seccompiler` directly (Linux).** Chosen
   over birdcage (GPL-3.0, FS-only — §0/H1), external wrappers (`bwrap`/`sandbox-exec`
   — external-binary requirement), and `extrasafe` (MIT but ~2yr stale). Both
   primitives are permissive and current; together they make `guarantees()` honest.
   The trait seam means the mechanism can still be swapped (e.g. add macOS) without
   touching `BashTool`.

3. **Config lives on the backend (clean break).** Execution config — sandbox/cwd,
   timeout, env allowlist, output cap, `rlimit`s — moves onto the backend.
   `BashTool` keeps only model-facing config (command allow/deny, schema). Makes the
   AC "swap the backend with no tool change" literally true. **Breaking change** to
   `BashTool`'s builder, accepted at 0.x with a minor bump + CHANGELOG (§10).

4. **`OsSandboxBackend` is fail-closed.** If the requested isolation cannot be
   established (Landlock unavailable — kernel < 5.13 or disabled; non-Linux target),
   `build()` returns `Err` — never a silent downgrade below what `guarantees()`
   advertises. The caller explicitly falls back to `HostBackend` if it chooses.

5. **`rlimit`s: `RLIMIT_CPU` + `RLIMIT_FSIZE` on by default; `RLIMIT_AS` opt-in**
   (review M2). `ResourceLimits` is a shared `exec`-module type reused by both
   backends.

## 3. Integration surface (existing APIs we build against)

Verified against the current tree:

- **`Tool<Ctx>`** (`core/src/tool.rs`) — `#[async_trait]`; `BashTool` keeps
  implementing it. Unchanged.
- **`ToolError`** (`core/src/tool.rs`) — `InvalidArgs { schema_errors }`,
  `Denied { reason }`, `Other(anyhow::Error)`, `#[non_exhaustive]`. **No new
  variant** needed.
- **`ToolOutput`** — `BashTool::invoke` still emits the same JSON, now assembled
  from a typed `ExecOutput`.
- **`Sandbox`** (`tools/src/sandbox.rs`) — unchanged; passed to a backend builder
  as cwd (and, for `OsSandboxBackend`, as the write-allowed root).
- **Current `bash.rs`** — its spawn/timeout/drain/reap machinery (incl.
  `process_group(0)` + `kill(-pgid, SIGKILL)` subtree kill) is **moved**, not
  rewritten, into the shared backend helper.
- **`web` feature precedent** — optional deps behind a Cargo feature, with the
  facade forwarding `tools-web = ["tools", "paigasus-helikon-tools/web"]`. Mirrored
  by `os-sandbox` / `tools-os-sandbox`.

## 4. Module layout

```
crates/paigasus-helikon-tools/src/
  exec/
    mod.rs          # ExecutionBackend trait; ExecRequest, ExecOutput,
                    # SandboxGuarantees, Isolation, ResourceLimits; spawn_capped helper
    host.rs         # HostBackend + HostBackendBuilder
    os_sandbox.rs   # OsSandboxBackend + builder + OsSandboxError
                    #   [cfg(all(feature = "os-sandbox", target_os = "linux"))]
  bash.rs           # slimmed to an Arc<dyn ExecutionBackend> adapter
  lib.rs            # re-exports the new public surface
```

Public re-exports from `lib.rs`: `ExecutionBackend`, `ExecRequest`, `ExecOutput`,
`SandboxGuarantees`, `Isolation`, `ResourceLimits`, `HostBackend`,
`HostBackendBuilder`, and (under `os-sandbox`, Linux) `OsSandboxBackend`,
`OsSandboxBackendBuilder`, `OsSandboxError`. Every `pub` item carries a `///` doc.

## 5. The `ExecutionBackend` trait & shared types

Object-safe, **not generic over `Ctx`**, so one value is shareable as
`Arc<dyn ExecutionBackend>` across agents of any context type.

```rust
#[async_trait]
pub trait ExecutionBackend: Send + Sync {
    /// Run one shell command to completion under this backend's containment.
    async fn run(&self, req: ExecRequest) -> Result<ExecOutput, ToolError>;

    /// What this backend actually enforces — surfaced in docs, the model-facing
    /// tool description, and traces. Describes *containment*, not approval.
    fn guarantees(&self) -> SandboxGuarantees;
}

#[non_exhaustive]
pub struct ExecRequest { pub command: String }   // ExecRequest::new(cmd); future: stdin, per-call env

#[non_exhaustive]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub truncated: bool,
}

#[non_exhaustive]
pub struct SandboxGuarantees {
    pub filesystem: Isolation,
    pub network: Isolation,
    pub syscalls: Isolation,
    pub label: &'static str,   // "host (no containment)" | "os-sandbox (landlock+seccomp)"
}

#[non_exhaustive]
pub enum Isolation {
    /// No OS enforcement on this axis — same access this process has.
    None,
    /// Enforced by an OS kernel mechanism (Landlock / seccomp-bpf).
    OsKernel,
}

#[non_exhaustive]
#[derive(Clone, Default)]
pub struct ResourceLimits {
    pub cpu_seconds: Option<u64>,          // RLIMIT_CPU   — default ON (derived from timeout)
    pub file_size_bytes: Option<u64>,      // RLIMIT_FSIZE — default ON (sane default)
    pub address_space_bytes: Option<u64>,  // RLIMIT_AS    — default OFF (opt-in; see §7 / review M2)
}
```

All four structs are `#[non_exhaustive]` so fields can grow non-breakingly.
`Isolation` carries only `None` and `OsKernel` here; future tiers
(`Virtualized` for SMA-416 microVM, `Proxied` for domain-level egress) are additive.

**Honesty note (review H1):** `Isolation::OsKernel` means "a kernel mechanism is
enforcing this axis." For `OsSandboxBackend`, `filesystem: OsKernel` is Landlock,
`syscalls: OsKernel` is a real seccomp-bpf policy, and `network: OsKernel` (when
denied) is the seccomp socket-family filter. The exact posture of each is documented
on the backend (§8) so the label never oversells — the trap H1 warned about.

## 6. `BashTool` as a thin adapter

`BashTool` holds `Arc<dyn ExecutionBackend>` + the command allow/deny prefix
matchers + the JSON schema:

```rust
impl<Ctx> Tool<Ctx> for BashTool<Ctx> {
    fn description(&self) -> &str { /* interpolates self.backend.guarantees().label */ }
    async fn invoke(&self, _ctx, args) -> Result<ToolOutput, ToolError> {
        let args: BashArgs = ...;                       // InvalidArgs on deserialize failure
        self.check_command_allowed(&args.command)?;     // -> ToolError::Denied
        let out = self.backend.run(ExecRequest::new(args.command)).await?;
        Ok(ToolOutput::new(json!({
            "stdout": out.stdout, "stderr": out.stderr,
            "exit_code": out.exit_code, "timed_out": out.timed_out,
            "truncated": out.truncated,
        })))
    }
}
```

Construction (the **breaking** reshape):

```rust
// before (SMA-328):  BashTool::builder(sandbox).timeout(..).env_allowlist(..).build()
// after (SMA-413):
let backend = HostBackend::builder(sandbox).timeout(..).build();   // Arc<dyn ExecutionBackend>
let bash = BashTool::builder(backend).deny_commands(["rm"]).build();
// or, no allow/deny:   BashTool::new(backend)
```

The model-facing `description()` interpolates `backend.guarantees().label`, so the
model is told which containment tier is live (satisfies "surfaced in docs + traces").
The "NOT a security boundary" wording stays for `HostBackend`.

## 7. `HostBackend` (default, hardened, still not a boundary)

Absorbs the current `bash.rs` execution logic **verbatim** and adds `rlimit`s.

```rust
HostBackend::builder(sandbox)        // sandbox.root() = cwd
    .timeout(Duration)               // default 30s
    .env_allowlist(["PATH","HOME"])  // default; replaces inherited env
    .max_output_bytes(1 << 20)       // default 1 MiB
    .rlimits(ResourceLimits { .. })  // CPU + FSIZE on; AS opt-in (§ below)
    .build() -> Arc<dyn ExecutionBackend>
```

- **Moved unchanged:** spawn (`sh -c` / `cmd /C`), `env_clear` + allowlist,
  concurrent capped pipe drains, bounded reaps, `process_group(0)` +
  `kill(-pgid, SIGKILL)` subtree kill on timeout. Factored into a shared internal
  `spawn_capped(...)` helper in `exec/mod.rs` that both backends call.
- **New — `rlimit`s (unix), via `Command::pre_exec` + `libc::setrlimit` before
  `exec`:**
  - `RLIMIT_CPU` — **on by default**, derived from the wall-timeout + a small margin
    (CPU backstop against a spin loop that ignores the wall kill).
  - `RLIMIT_FSIZE` — **on by default**, a sane max-bytes-written cap.
  - `RLIMIT_AS` — **opt-in / default `None`** (review M2): an address-space cap
    spuriously kills threaded and `mmap`-heavy programs; documented as approximate.
  - Each field is `Option`; `None` leaves the inherited limit. `RLIMIT_NPROC` is
    not set (per-UID → can starve unrelated host work). Concrete default numbers
    finalized in the plan.
- **Platform reality:** `pre_exec`/`setrlimit` are unix-only; on Windows the backend
  still runs and `rlimit`s are a documented no-op.

`guarantees()` → `{ filesystem: None, network: None, syscalls: None,
label: "host (no containment)" }` on **every** platform. `rlimit`s are resource
hygiene, not access containment, so they deliberately do **not** upgrade any axis to
`OsKernel`.

## 8. `OsSandboxBackend` (real OS containment — Linux, via `landlock` + `seccompiler`)

Feature-gated behind `os-sandbox`; the type + its deps compile **only on Linux**
(`[target.'cfg(target_os = "linux")'.dependencies]`), so FS/Bash-only and non-Linux
consumers never pull `landlock`/`seccompiler`. The trait and `HostBackend` stay
always-available.

### 8.1 Builder

```rust
// #[cfg(all(feature = "os-sandbox", target_os = "linux"))]
OsSandboxBackend::builder(sandbox)
    .timeout(Duration)
    .env_allowlist([..])
    .max_output_bytes(..)
    .rlimits(..)                  // shared ResourceLimits — defense in depth inside the jail
    .allow_network(false)         // default DENY; all-or-nothing until the proxy follow-up
    .read_paths([..])             // extra read-only exceptions (e.g. a toolchain dir)
    .build() -> Result<Arc<dyn ExecutionBackend>, OsSandboxError>
```

### 8.2 Execution & the fork-safe application model (review M1)

Same `spawn_capped(...)` path as `HostBackend`, but with a `pre_exec` hook that
applies the jail. Async-signal-safety dictates a **build-in-parent / apply-in-child**
split:

- **In the parent, before fork** (allocation/locks fine): create the Landlock
  ruleset (the `landlock` crate's `Ruleset` → `RulesetCreated`, holding a ruleset fd)
  with the path rules from §8.3; **compile** the seccomp filter to a
  `seccompiler::BpfProgram` (a `Vec<sock_filter>`). Both the ruleset fd and the
  compiled BPF bytes are moved into the `pre_exec` closure.
- **In the child `pre_exec`** (async-signal-safe — raw syscalls only, no
  allocation): `prctl(PR_SET_NO_NEW_PRIVS, 1)`; load the **pre-compiled** seccomp
  program via `seccomp(2)`/`prctl`; `landlock_restrict_self(ruleset_fd)`; apply
  `setrlimit`s; return so `exec` proceeds. Nothing here allocates or locks.

This is why we use the primitives directly rather than a `lock()`-the-current-process
library: it lets us do the unsafe work fork-safely in `pre_exec`. The fork-safe
contract is documented at the `pre_exec` call site.

### 8.3 What is enforced

- **Filesystem (Landlock, `filesystem: OsKernel`):** write+read access for the
  sandbox root only; read+exec access for the minimal system paths a shell needs
  (`/bin`, `/usr`, `/lib*`, loader/`resolv` config, plus any `read_paths`).
  Everything else is denied by the **kernel** — `echo x > /etc/passwd` or any write
  to an absolute path outside the root fails at the OS layer, independent of our
  path validation.
- **Syscalls (seccomp-bpf, `syscalls: OsKernel`):** a **targeted-deny** filter (v1
  posture, documented): allow by default, deny a defined dangerous set — `ptrace`,
  `mount`/`umount`, `kexec_load`, `bpf`, `unshare`/`setns` into new namespaces, etc.
  A stricter full **allowlist** filter is noted as future hardening (a complete
  allowlist for arbitrary shell + coreutils is fragile). The docs state the exact
  posture so `syscalls: OsKernel` is not oversold.
- **Network (`network: OsKernel` when denied):** by default the same seccomp filter
  rejects `socket(2)` for `AF_INET`/`AF_INET6` (and `AF_PACKET`), so no IP egress
  is possible; `AF_UNIX` is left allowed so local IPC still works.
  `allow_network(true)` omits that rule → `network: None`. Domain-level allow/deny is
  **out of scope** here (the proxy follow-up); a future refinement can use Landlock
  ABI v4 per-port network rules (kernel 6.7+).
- **No namespaces required** (review L2): Landlock covers fs and seccomp covers
  syscalls/network, so we need **no** user/mount/net namespaces and therefore **no**
  unprivileged-userns dependency — simplifying the deployment matrix to "Landlock ≥
  5.13 + seccomp."

### 8.4 Fail-closed construction & platform matrix

`build()` returns `Err(OsSandboxError)` when isolation can't be established: Landlock
ABI unavailable (kernel < 5.13 or disabled), a missing required system path, or a
Landlock/seccomp setup error. Never a silent downgrade. On non-Linux targets the
type does not exist (cfg'd out) — documented as Linux-only.

`guarantees()` reflects what is **actually** active:
`{ filesystem: OsKernel, syscalls: OsKernel,
network: OsKernel when denied / None when allowed,
label: "os-sandbox (landlock+seccomp)" }`.

## 9. Error model

| Condition | Type |
|-----------|------|
| Bash args fail schema/deserialize | `ToolError::InvalidArgs` (recoverable) |
| Command blocked by allow/deny list | `ToolError::Denied { reason }` |
| Shell spawn / I/O failure during `run` | `ToolError::Other(anyhow)` |
| Non-zero exit / timed-out / truncated | **not errors** — fields on `ExecOutput` |
| `OsSandboxBackend` can't establish isolation | `OsSandboxError` (construction, **not** a `ToolError`) |

`OsSandboxError` is a new crate-local `thiserror` / `#[non_exhaustive]` enum,
parallel to `SandboxError` (which covers `Sandbox::open`). **No
`paigasus-helikon-core` change** — so no 5-step ascend and no manual facade bump.

## 10. Dependencies & release mechanics

**Dependencies** (root `[workspace.dependencies]`, referenced via `dep.workspace = true`),
optional and **Linux-target-gated** in `-tools`:

- `landlock` — `MIT OR Apache-2.0`, latest `0.4.5` (May 2026). Filesystem rules +
  `restrict_self`.
- `seccompiler` — `Apache-2.0 OR BSD-3-Clause`, latest `0.5.0` (Mar 2025). Compiles
  the syscall/network BPF filter.

```toml
# crates/paigasus-helikon-tools/Cargo.toml
[features]
os-sandbox = ["dep:landlock", "dep:seccompiler"]   # Linux only

[target.'cfg(target_os = "linux")'.dependencies]
landlock    = { workspace = true, optional = true }
seccompiler = { workspace = true, optional = true }
```

**`cargo deny`:** both deps are OR-licensed into the existing allowlist
(`MIT`, `Apache-2.0`, `BSD-3-Clause` are all already allowed in `deny.toml`), so
**no new license-allowlist entry is expected**. Confirm with `cargo deny check` and
commit the `Cargo.lock` update.

**Release** (`-tools` is at `0.1.6`):

1. **Breaking API reshape** (config-on-backend) → `0.1.6 → 0.2.0`. Flag via a
   `feat(tools)!:` subject or a `BREAKING CHANGE:` footer so release-plz selects the
   minor bump (0.x breaking = minor).
2. **No core change** → standard release-plz flow; **no** 5-step ascend, **no**
   manual facade bump.
3. **Facade:** add `tools-os-sandbox = ["tools", "paigasus-helikon-tools/os-sandbox"]`
   mirroring `tools-web`. (Expose through the facade for consistency with `web`.)
4. Commit the `Cargo.lock` update from the new deps.

## 11. Follow-up tickets

- **Network egress proxy (domain-level).** Promote the SMA-412 web domain/SSRF
  policy (`host_allowed`, `ip_blocked`, `GuardedResolver`, currently crate-private in
  `web/http.rs` behind `web`) to a **public shared policy type**, and stand up a
  CONNECT-proxy the sandboxed child is pointed at (`HTTP(S)_PROXY`), enforcing that
  policy. Adds `Isolation::Proxied` to `guarantees()`. References this spec.
- **macOS Seatbelt backend.** A macOS `OsSandboxBackend` path (via `sandbox-exec` or
  a maintained Seatbelt binding) behind the same trait, no `BashTool` change.

## 12. Testing & the demo

- **Backend wiring (portable, CI):** a mock `ExecutionBackend` proves `BashTool`
  calls `run`, maps `ExecOutput`→`ToolOutput`, threads allow/deny → `Denied`, and
  interpolates `guarantees().label` into `description()`; and that swapping the mock
  for `HostBackend` needs **zero** tool/agent changes (the headline AC).
- **`HostBackend` (unix):** existing `tests/bash.rs` cases (timeout, subtree kill,
  env scrub, output cap, exit codes) move over; **new** `rlimit` tests — a CPU spin
  loop dies to `RLIMIT_CPU`; an oversized write dies to `RLIMIT_FSIZE`.
- **`OsSandboxBackend` (`os-sandbox`, `#[cfg(target_os = "linux")]`) — the AC tests:**
  a command writing outside the sandbox root is blocked **at the OS layer** (the
  write fails even though path validation would have allowed it), while writing
  inside the root succeeds; with `allow_network(false)` an outbound TCP connection
  fails; `guarantees()` reports `OsKernel` on fs/syscalls (+ network when denied).
- **CI honesty (review):** Landlock/seccomp availability on the GitHub `ubuntu-latest`
  runner is **verified during planning** (kernel ≥ 5.13 + Landlock enabled). Where a
  runner cannot enforce, the test is `#[ignore]`'d with an explicit reason — **never
  silently skipped to green** (a sandbox test that passes because the sandbox is
  inactive is worse than no test).
- **Example (manual, not CI):** extend the SMA-328 sandbox example to build an
  `OsSandboxBackend` on Linux, print `guarantees()`, and show a blocked-write
  attempt, behind the `os-sandbox` feature.

## 13. Docs (same PR, per CLAUDE.md)

Update the mdBook tools/sandbox page: the **containment ≠ approval ≠
resource-capping** axis; the backends, **leading with `OsSandboxBackend`** on Linux
(review L1) and its fail-closed fallback story, then `HostBackend` as the
default-but-unconfined option with its "NOT a security boundary" label; the
`guarantees()` tiers and exactly what each enforces; and the kernel-feature matrix
(Landlock ≥ 5.13 + seccomp; no namespaces). Note the proxy + macOS backends as
forthcoming. `mdbook build docs/book` stays clean (`warning-policy = "error"`).
Crate-level + `///` docs on every new `pub` item.

## 14. Out of scope (YAGNI)

- The deny-by-default **domain-level** network egress proxy + the SMA-412 policy
  promotion (§11, follow-up).
- **macOS Seatbelt** backend (§11, follow-up) — Linux-first matches the AC.
- **Namespaces / userns** isolation (§8.3 — Landlock+seccomp cover the AC without
  them).
- microVM / Firecracker backend (SMA-416, which this ticket *blocks*).
- Container (Docker) backend.
- Per-call env overrides / stdin on `ExecRequest` (the `#[non_exhaustive]` struct
  reserves room; not implemented now).

## 15. Acceptance criteria (restated against this design)

- `BashTool` runs against any `ExecutionBackend`; swapping `HostBackend` ↔
  `OsSandboxBackend` (↔ a mock) needs **no** change to tool or agent code.
- `HostBackend`: a runaway command is killed on timeout **including child
  processes** (shipped) and **CPU/file-size `rlimit`s are enforced** (new;
  `RLIMIT_AS` available but opt-in).
- `OsSandboxBackend` (Linux): a command writing outside the sandbox root is blocked
  **at the OS layer**; with network denied, outbound network fails. (Domain-level
  allowlisting and macOS are the documented follow-ups; this PR's network is binary.)
- `guarantees()` exposes which isolation tier is active and **accurately** reflects
  what each backend enforces, surfaced in the model-facing `description()`.
- All CI gates green (fmt, clippy incl. `--all-features`, the test matrix with the
  `cfg`/feature-split backend tests, docs, doc-coverage, commits, pr-title, audit,
  deny), and `mdbook build` clean.
```
