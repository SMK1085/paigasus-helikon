# SMA-413 — `paigasus-helikon-tools`: pluggable `ExecutionBackend` for Bash

**Status:** approved (brainstorm) — pending written-spec review
**Ticket:** [SMA-413](https://linear.app/smaschek/issue/SMA-413/paigasus-helikon-tools-pluggable-executionbackend-for-bash-host-os)
**Milestone:** Composition & Extensibility
**Branch:** `feature/sma-413-paigasus-helikon-tools-pluggable-executionbackend-for-bash`
**Date:** 2026-06-16

## 1. Summary

Introduce **containment as a first-class, swappable axis, separate from approval**.
Today `BashTool` (SMA-328) is *soft confinement only* — cwd pinned, env allowlist,
timeout, output cap — and a spawned shell escapes the `cap-std` jail; the only real
control is the runner's `PermissionPolicy` / `DenyRule::tool("Bash")`, which is an
*approval* gate, not *containment*.

This ticket extracts an object-safe `ExecutionBackend` trait that `BashTool` runs
against, and ships two backends:

- **`HostBackend`** — today's behaviour, kept as the **default** and **kept
  documented as "NOT a security boundary"**, hardened with CPU/memory/file-size
  `rlimit`s (process-group subtree-kill on timeout already shipped in SMA-328).
- **`OsSandboxBackend`** — real OS *process* containment of the filesystem and
  syscalls via the pure-Rust `birdcage` crate (Linux Landlock + seccomp +
  namespaces; macOS Seatbelt), with an all-or-nothing network toggle.

Swapping the backend needs **no change to tool or agent code**.

`-tools` is already released at `0.1.5`; this is a normal release-plz flow with a
**breaking** API reshape → `0.2.0`. No `paigasus-helikon-core` change is required.

## 2. Scope decisions (resolved during brainstorming)

These were made explicitly and drive the rest of the design.

1. **PR scope: trait + `HostBackend` + `OsSandboxBackend` (fs + syscalls); defer
   the network proxy.** This PR delivers the `ExecutionBackend` seam, DI into
   `BashTool`, `HostBackend` hardening, and `OsSandboxBackend` with OS-enforced
   **filesystem + syscall** containment plus an **all-or-nothing** network toggle.
   The ticket's deny-by-default **domain-level egress proxy** is split into a
   follow-up (§11): it requires promoting the SMA-412 web domain/SSRF policy
   (currently crate-private in `web/http.rs`) to a public shared type **and**
   standing up a CONNECT-proxy process — its own subsystem. Consequence: the
   ticket AC "outbound network to a non-allowlisted domain is denied" is
   re-scoped to that follow-up; this PR's network story is binary (deny-all or
   allow-all).

2. **Enforcement mechanism: the pure-Rust `birdcage` crate.** One cross-platform
   dependency wraps Linux Landlock+seccomp+namespaces and macOS Seatbelt behind
   a single API; no external binaries for consumers (rejected: `bwrap` /
   `sandbox-exec` wrappers, and hand-rolled `landlock`/`seccompiler`/`unshare`).
   The trait seam means the mechanism can be swapped later without touching
   `BashTool`. Exact API/license/maintenance verified during planning (§9, §10).

3. **Config lives on the backend (clean break).** Execution config — sandbox/cwd,
   timeout, env allowlist, output cap, `rlimit`s — moves onto the backend.
   `BashTool` keeps only model-facing config (command allow/deny, schema). This
   makes the AC "swap the backend with no tool change" literally true and gives
   each concern one home. It is a **breaking change** to `BashTool`'s builder,
   accepted at 0.x with a minor bump + CHANGELOG (§10).

4. **`OsSandboxBackend` is fail-closed.** If the requested isolation cannot be
   established (Landlock absent on an old kernel, unsupported platform, missing
   system path), `build()` returns `Err` — never a silent downgrade to weaker
   containment than `guarantees()` advertises. The caller explicitly falls back
   to `HostBackend` if it chooses to proceed unconfined.

5. **`rlimit`s are on by default (opt-out).** Hardening should be on unless a
   caller deliberately relaxes it. `ResourceLimits` is a shared `exec`-module
   type reused by both backends.

## 3. Integration surface (existing APIs we build against)

Verified against the current tree:

- **`Tool<Ctx>`** (`core/src/tool.rs`) — `#[async_trait]`; `BashTool` keeps
  implementing it. Unchanged.
- **`ToolError`** (`core/src/tool.rs`) — `InvalidArgs { schema_errors }`,
  `Denied { reason }`, `Other(anyhow::Error)`. **No new variant** is needed;
  `run` returns these. (`Denied` already exists from SMA-328.)
- **`ToolOutput`** — `BashTool::invoke` still emits the same JSON
  (`{ stdout, stderr, exit_code, timed_out, truncated }`), now assembled from a
  typed `ExecOutput`.
- **`Sandbox`** (`tools/src/sandbox.rs`) — unchanged; passed to a backend builder
  as cwd (and, for `OsSandboxBackend`, as the write-allowed root).
- **Current `bash.rs`** — its spawn/timeout/drain/reap machinery (incl.
  `process_group(0)` + `kill(-pgid, SIGKILL)` subtree kill) is **moved**, not
  rewritten, into the shared backend helper.
- **`web` feature precedent** — optional deps behind a Cargo feature; mirrored by
  the new `os-sandbox` feature.

## 4. Module layout

```
crates/paigasus-helikon-tools/src/
  exec/
    mod.rs          # ExecutionBackend trait; ExecRequest, ExecOutput,
                    # SandboxGuarantees, Isolation, ResourceLimits; spawn_capped helper
    host.rs         # HostBackend + HostBackendBuilder
    os_sandbox.rs   # OsSandboxBackend + builder + OsSandboxError  [cfg(feature="os-sandbox")]
  bash.rs           # slimmed to an Arc<dyn ExecutionBackend> adapter
  lib.rs            # re-exports the new public surface
```

Public re-exports from `lib.rs`: `ExecutionBackend`, `ExecRequest`, `ExecOutput`,
`SandboxGuarantees`, `Isolation`, `ResourceLimits`, `HostBackend`,
`HostBackendBuilder`, and (under `os-sandbox`) `OsSandboxBackend`,
`OsSandboxBackendBuilder`, `OsSandboxError`. Every `pub` item carries a `///` doc
(workspace `missing_docs = "warn"` + `-D warnings` + doc-coverage gate).

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
    pub label: &'static str,   // e.g. "host (no containment)", "os-sandbox (landlock+seccomp+namespaces)"
}

#[non_exhaustive]
pub enum Isolation {
    /// No OS enforcement on this axis — same access this process has.
    None,
    /// Enforced by an OS kernel mechanism (Landlock/seccomp/namespaces/Seatbelt).
    OsKernel,
}

#[non_exhaustive]
#[derive(Clone, Default)]
pub struct ResourceLimits {
    pub cpu_seconds: Option<u64>,          // RLIMIT_CPU
    pub address_space_bytes: Option<u64>,  // RLIMIT_AS  (the "memory" cap)
    pub file_size_bytes: Option<u64>,      // RLIMIT_FSIZE
}
```

`ExecRequest`/`ExecOutput`/`SandboxGuarantees`/`ResourceLimits` are `#[non_exhaustive]`
so fields can grow without a breaking change.

`Isolation` deliberately has only `None` and `OsKernel` in this PR. Future tiers
(e.g. `Virtualized` for the microVM follow-up SMA-416, `Proxied` for domain-level
egress) are additive on the `#[non_exhaustive]` enum.

## 6. `BashTool` as a thin adapter

`BashTool` holds `Arc<dyn ExecutionBackend>` + the command allow/deny prefix
matchers + the JSON schema. Its responsibilities shrink to: parse args, check
allow/deny, delegate to the backend, map the result.

```rust
impl<Ctx> Tool<Ctx> for BashTool<Ctx> {
    fn description(&self) -> &str { /* interpolates self.backend.guarantees().label */ }
    async fn invoke(&self, _ctx, args) -> Result<ToolOutput, ToolError> {
        let args: BashArgs = ...;                 // InvalidArgs on deserialize failure
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
let bash = BashTool::builder(backend)            // builder now carries only allow/deny + schema
    .deny_commands(["rm"])
    .build();
// or, no allow/deny:   BashTool::new(backend)
```

The model-facing `description()` interpolates `backend.guarantees().label`, so the
model is told which containment tier is live — satisfying "surfaced in
docs + traces" at no extra cost. The "NOT a security boundary" wording stays for
`HostBackend`.

## 7. `HostBackend` (default, hardened, still not a boundary)

Absorbs the current `bash.rs` execution logic **verbatim** and adds `rlimit`s.

```rust
HostBackend::builder(sandbox)        // sandbox.root() = cwd
    .timeout(Duration)               // default 30s
    .env_allowlist(["PATH","HOME"])  // default; replaces inherited env
    .max_output_bytes(1 << 20)       // default 1 MiB
    .rlimits(ResourceLimits { .. })  // on by default (§ defaults below)
    .build() -> Arc<dyn ExecutionBackend>
```

- **Moved unchanged:** spawn (`sh -c` / `cmd /C`), `env_clear` + allowlist,
  concurrent capped pipe drains, bounded reaps, `process_group(0)` +
  `kill(-pgid, SIGKILL)` subtree kill on timeout. Factored into a shared internal
  `spawn_capped(...)` helper in `exec/mod.rs` that both backends call.
- **New — `rlimit`s (unix):** applied in the child via `Command::pre_exec` calling
  `libc::setrlimit` **before** `exec`. **Defaults (opt-out):** `RLIMIT_CPU` set
  from the wall-timeout + a small margin (CPU backstop against a spin loop that
  ignores the wall kill), `RLIMIT_AS` set to a sane default address-space cap, and
  `RLIMIT_FSIZE` set to a sane default. Each field is `Option`; `None` leaves the
  inherited limit. `RLIMIT_NPROC` is **not** set by default (per-UID, not
  per-process → can starve unrelated host work). Concrete default numbers are
  finalized in the plan.
- **Platform reality:** `pre_exec`/`setrlimit` are unix-only; on Windows the
  backend still runs and `rlimit`s are a documented no-op.

`guarantees()` → `{ filesystem: None, network: None, syscalls: None,
label: "host (no containment)" }` on **every** platform. `rlimit`s are resource
hygiene, not access containment, so they deliberately do **not** upgrade any axis
to `OsKernel` — keeping "containment ≠ approval ≠ resource-capping" honest.

## 8. `OsSandboxBackend` (real OS containment, via `birdcage`)

Feature-gated behind `os-sandbox` (mirrors `web`), so FS/Bash-only consumers never
pull `birdcage`. The trait and `HostBackend` stay always-available.

```rust
// #[cfg(feature = "os-sandbox")]
OsSandboxBackend::builder(sandbox)
    .timeout(Duration)
    .env_allowlist([..])
    .max_output_bytes(..)
    .rlimits(..)                  // shared ResourceLimits — defense in depth inside the jail
    .allow_network(false)         // default DENY; all-or-nothing until the proxy follow-up
    .read_paths([..])             // extra read-only exceptions (e.g. a toolchain dir)
    .build() -> Result<Arc<dyn ExecutionBackend>, OsSandboxError>
```

- **Execution path:** the same `spawn_capped(...)` helper as `HostBackend`, but the
  child applies `birdcage` restrictions in `pre_exec` before `exec`'ing the shell.
- **Filesystem:** write+read exception for the sandbox root only; read+exec
  exceptions for the minimal system paths a shell needs (`/bin`, `/usr`, `/lib*`,
  loader/`resolv` config, plus any `read_paths`). Everything else is denied by the
  **kernel** — `echo x > /etc/passwd` or any write to an absolute path outside the
  root fails at the OS layer, independent of our path validation.
- **Syscalls:** `birdcage`'s default seccomp filter.
- **Network:** denied unless `allow_network(true)`. Domain-level allow/deny is
  **out of scope** here (deferred proxy).
- **Fail-closed construction:** `build()` returns `Err(OsSandboxError)` when the
  requested isolation can't be established (no Landlock, unsupported platform,
  missing system path). Never a silent downgrade.
- **Non-Linux/macOS (e.g. Windows):** the `os-sandbox` feature does not build
  `birdcage`/`OsSandboxBackend` there — documented as Linux+macOS only.

`guarantees()` reflects what is **actually** active on the platform it built on:
`{ filesystem: OsKernel, syscalls: OsKernel,
network: OsKernel when denied / None when allowed,
label: "os-sandbox (landlock+seccomp+namespaces)" | "os-sandbox (seatbelt)" }`.

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

**Dependencies** (root `[workspace.dependencies]`, referenced via `dep.workspace = true`):

- `birdcage` — optional, behind `os-sandbox`. Pin the current major. **Verify in
  planning:** exact API (pre_exec vs spawn helper; network-toggle semantics),
  license, maintenance status, and that it + its transitive deps (`landlock`,
  `seccompiler`, …) pass `cargo deny` — add an allowlist entry only if it actually
  fails. If `birdcage` proves unsuitable, the trait seam lets us swap mechanism
  with no `BashTool` change.

```toml
[features]
os-sandbox = ["dep:birdcage"]   # Linux + macOS
```

**Release** (`-tools` is already at `0.1.5`):

1. **Breaking API reshape** (config-on-backend) → `0.1.5 → 0.2.0`. Flag the break
   via a `feat(tools)!:` subject or a `BREAKING CHANGE:` footer so release-plz
   selects the minor bump (0.x breaking = minor).
2. **No core change** → standard release-plz flow; **no** 5-step ascend, **no**
   manual facade bump (the facade-drift caveat applies only to the same-PR manual
   core bump path, which we don't hit).
3. **Facade feature:** add `os-sandbox` to the facade's feature map only if we
   surface `OsSandboxBackend` through `paigasus-helikon`'s `tools` feature.
   **Open item resolved in the plan:** expose through the facade vs direct-dep
   only. Default lean: expose it (consistency with `web`).
4. Commit the `Cargo.lock` update from the new deps.

## 11. Follow-up ticket (network egress proxy)

File under **Composition & Extensibility**: deny-by-default **domain-level**
network egress for `OsSandboxBackend`. Two parts: (a) promote the SMA-412 web
domain/SSRF policy (`host_allowed`, `ip_blocked`, `GuardedResolver`, currently
crate-private in `web/http.rs` behind the `web` feature) to a **public shared
policy type**; (b) stand up a CONNECT-proxy process the sandboxed child is pointed
at (e.g. via `HTTP(S)_PROXY`), enforcing that policy, with the sandbox's network
namespace permitting egress only to the proxy. Adds an `Isolation::Proxied` (or a
richer network tier) to `guarantees()`. References this spec for the trait/error
conventions.

## 12. Testing & the demo

- **Backend wiring (portable, CI):** a mock `ExecutionBackend` proves `BashTool`
  calls `run`, maps `ExecOutput`→`ToolOutput`, threads allow/deny → `Denied`, and
  interpolates `guarantees().label` into `description()`; and that swapping the
  mock for `HostBackend` needs **zero** tool/agent changes (the headline AC).
- **`HostBackend` (unix):** the existing `tests/bash.rs` cases (timeout, subtree
  kill, env scrub, output cap, exit codes) move over; **new** `rlimit` tests — a
  CPU spin loop dies to `RLIMIT_CPU`; an over-allocation dies to `RLIMIT_AS`.
- **`OsSandboxBackend` (`os-sandbox`, `#[cfg(target_os)]`) — the AC tests:** a
  command writing outside the sandbox root is blocked **at the OS layer** (the
  write fails even though path validation would have allowed it), while writing
  inside the root succeeds; with `allow_network(false)` an outbound connection
  fails; `guarantees()` reports `OsKernel` on fs/syscalls.
- **CI honesty:** Landlock/seccomp/Seatbelt availability on GitHub runners is
  **verified during planning**. Where a runner cannot enforce, the test is
  `#[ignore]`'d with an explicit reason — **never silently skipped to green**.
- **Example (manual, not CI):** extend/clone the SMA-328 sandbox example to build
  an `OsSandboxBackend` and show `guarantees()` plus a blocked-write attempt,
  behind the `os-sandbox` feature.

## 13. Docs (same PR, per CLAUDE.md)

Update the mdBook tools/sandbox page: the **containment ≠ approval** axis, the
three backends (`Host`/`OsSandbox`, with the proxy noted as forthcoming), and the
`guarantees()` tiers. `mdbook build docs/book` stays clean
(`warning-policy = "error"`). Crate-level + `///` docs on every new `pub` item.

## 14. Out of scope (YAGNI)

- The deny-by-default **domain-level** network egress proxy + the SMA-412 policy
  promotion (§11, follow-up).
- microVM / Firecracker backend (SMA-416, this ticket *blocks* it).
- Container (Docker) backend.
- Per-call env overrides / stdin on `ExecRequest` (the `#[non_exhaustive]` struct
  reserves room; not implemented now).

## 15. Acceptance criteria (restated against this design)

- `BashTool` runs against any `ExecutionBackend`; swapping `HostBackend` ↔
  `OsSandboxBackend` (↔ a mock) needs **no** change to tool or agent code.
- `HostBackend`: a runaway command is killed on timeout **including child
  processes** (shipped) and **CPU/memory `rlimit`s are enforced** (new).
- `OsSandboxBackend` (Linux): a command writing outside the sandbox root is
  blocked **at the OS layer**; with network denied, outbound network fails.
  (Domain-level allowlisting is the follow-up.)
- `guarantees()` exposes which isolation tier is active, and it is surfaced in the
  model-facing `description()`.
- All CI gates green (fmt, clippy incl. `--all-features`, the test matrix with the
  `cfg`/feature-split backend tests, docs, doc-coverage, commits, pr-title, audit,
  deny), and `mdbook build` clean.
