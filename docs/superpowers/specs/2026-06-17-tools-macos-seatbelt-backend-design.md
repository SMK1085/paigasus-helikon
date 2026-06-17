# SMA-426 — `paigasus-helikon-tools`: macOS Seatbelt `ExecutionBackend` for Bash

**Status:** approved (brainstorm) — pending written-spec review
**Ticket:** [SMA-426](https://linear.app/smaschek/issue/SMA-426/paigasus-helikon-tools-macos-seatbelt-executionbackend-for-bash)
**Milestone:** Composition & Extensibility
**Branch:** `feature/sma-426-paigasus-helikon-tools-macos-seatbelt-executionbackend-for`
**Date:** 2026-06-17
**Builds on:** [SMA-413 design](2026-06-16-tools-execution-backend-design.md) (the `ExecutionBackend` trait + Linux Landlock/seccomp backend) — see its §8 and §11.

## 1. Summary

SMA-413 shipped the object-safe `ExecutionBackend` trait plus an `OsSandboxBackend`
that enforces filesystem + syscall + network containment **on Linux only** (via the
permissive `landlock` + `seccompiler` crates). It deferred macOS because **no
maintained, permissively-licensed, pure-Rust Seatbelt binding exists** — the one
crate that bundled Linux + macOS (`birdcage`) is GPL-3.0, which fails our published
`Apache-2.0 OR MIT` license and the `cargo deny` gate.

This ticket adds the **macOS** containment path behind the **same trait**, with the
**same public type names** (`OsSandboxBackend`, `OsSandboxBackendBuilder`,
`OsSandboxError`), cfg'd so exactly one backend compiles per target. Containment is
enforced by **Seatbelt** via the `sandbox-exec` binary. Swapping the backend needs
**no change to `BashTool` or agent code** — the headline acceptance criterion.

Two properties make this lighter than the Linux side:

1. **Zero new crate dependencies.** `sandbox-exec` is an OS binary, the profile is a
   string, and resource limits use `libc` (already a unix dep). This sidesteps the
   license/`cargo deny` problem that disqualified birdcage.
2. **Natively testable on the dev machine** (macOS), unlike the Linux backend which
   required cross-compile checks.

`-tools` is at `0.2.1`; this is a purely **additive** change (new platform path, same
type names) → **patch** bump `0.2.2` through release-plz's normal flow. **No
`paigasus-helikon-core` change.**

## 2. Decisions taken in the brainstorm

1. **Mechanism: `sandbox-exec` wrapper (Option A), not `sandbox_init` FFI (Option B).**
   Run the command as `sandbox-exec -D ROOT=<root> -p <profile> sh -c <command>`. The
   OS binary compiles and applies the SBPL profile in its own fresh process before
   exec'ing the shell. Option B is rejected: `sandbox_init` compiles the profile (it
   allocates) and is therefore **not async-signal-safe**, so calling it in a post-`fork`
   `pre_exec` hook inside multithreaded Tokio is a deadlock hazard; the
   compile-in-parent / apply-in-child split only exists as **private Apple SPI**
   (`sandbox_compile_*`/`sandbox_apply`). `sandbox-exec` avoids both, needs no new deps,
   and the trait seam keeps FFI swappable later. `sandbox-exec` is Apple-deprecated but
   ships on every macOS and is widely used (e.g. Nix); the deprecation caveat is
   documented.

2. **Filesystem posture: strict read-deny parity with the Linux Landlock rules.**
   `(deny default)`; allow **read** of a curated macOS system-read set (dyld shared
   cache, `/usr`, `/bin`, `/System`, `/Library`, `/etc`, needed `/dev` nodes); allow
   **read+write** under the sandbox root only. This matches SMA-413's Linux posture
   (which allows reads of a system path set and denies the rest, *not* "deny all
   reads"). **Documented fallback:** if a working shell proves too fragile under full
   read-deny on the macOS CI runner, fall back to write-focused (read-allow-all,
   write-only-root) with `filesystem: OsKernel` honestly documented as
   write-containment. The fallback is a documented contingency, not the plan.

3. **`guarantees().syscalls` reports `None` for Seatbelt (do not oversell).** Seatbelt
   is an operation-based MAC (it gates file / network / mach / process / sysctl
   *operations*), **not** a syscall allowlist/denylist like seccomp, and our profile
   allows `process*` so the raw syscall surface is unconstrained. `filesystem` and
   `network` are genuinely `OsKernel`; `syscalls` is honestly `None`. The
   `label: "os-sandbox (seatbelt)"` makes the mechanism explicit so the `OsKernel`
   axes are never read as seccomp-grade syscall filtering.

4. **CI honesty: runtime-guard skip, consistent with the Linux backend.** Tests use a
   `seatbelt_unavailable()` guard that calls `build()` and, on `Err`, does a loud
   `eprintln!("SKIP: …")` and returns — **never silently green**. This matches the
   existing Linux `landlock_unavailable()` pattern; the ticket AC's `#[ignore = "…"]`
   wording is satisfied in spirit (a sandbox test that passes because the sandbox is
   inactive is worse than no test). Planning verifies that GitHub's macOS runner
   actually *enforces* Seatbelt, so the tests pass-because-enforced rather than skip.

## 3. Integration surface (existing APIs we build against)

Verified against the current tree (`crates/paigasus-helikon-tools/src/exec/`):

- **`ExecutionBackend`** (`exec/mod.rs`) — object-safe `run()` + `guarantees()`.
  Unchanged. The macOS backend implements it identically to the Linux one.
- **`spawn_capped` / `build_command`** (`exec/mod.rs`) — the shared spawn /
  cwd / env-scrub / capped-pipe-drain / timeout / subtree-kill / reap helper. Today
  `build_command` hardcodes `sh -c <command>`. **Generalized** here (§5) to accept an
  argv prefix; this is the **only** edit that touches Linux-compiled code.
- **`ExecConfig`, `ExecRequest`, `ExecOutput`, `SandboxGuarantees`, `Isolation`,
  `ResourceLimits`, `apply_rlimits`** (`exec/mod.rs`) — reused unchanged.
  `apply_rlimits` (async-signal-safe `setrlimit`) already compiles on macOS (it is
  `#[cfg(unix)]`).
- **`Sandbox`** (`sandbox.rs`) — unchanged; passed to the builder as cwd and the
  write-allowed root.
- **Linux `OsSandboxBackend`** (`exec/os_sandbox.rs`) — unchanged; the naming /
  builder-method parity (§4, §6) is modelled on it.
- **`os-sandbox` feature** — already declared; on macOS it pulls **no** deps today
  (`landlock`/`seccompiler` are `[target.'cfg(target_os = "linux")'.dependencies]`).
  The macOS backend adds none.

## 4. Module layout

```
crates/paigasus-helikon-tools/src/exec/
  mod.rs                  # generalize build_command (§5); wire the macOS cfg branch
  host.rs                 # unchanged behaviour (empty-prefix call site)
  os_sandbox.rs           # Linux Landlock+seccomp (unchanged except the empty-prefix call site)
  os_sandbox_seatbelt.rs  # NEW — macOS Seatbelt backend + builder + OsSandboxError
```

`mod.rs` wiring — the macOS module re-exports the **same names** as the Linux one, so
exactly one compiles per target and consumer code is byte-identical across OSes:

```rust
#[cfg(all(feature = "os-sandbox", target_os = "macos"))]
mod os_sandbox_seatbelt;
#[cfg(all(feature = "os-sandbox", target_os = "macos"))]
pub use os_sandbox_seatbelt::{OsSandboxBackend, OsSandboxBackendBuilder, OsSandboxError};
```

(The Linux branch keeps its existing
`#[cfg(all(feature = "os-sandbox", target_os = "linux", any(x86_64, aarch64)))]`
gate.) Every new `pub` item carries a `///` doc.

## 5. The one shared-code change: generalize `build_command`

`spawn_capped` stays the orchestrator (cwd, env scrub, pipe drains, timeout,
process-group subtree kill, reaping). Only **how the base command is constructed**
becomes pluggable, via an argv **prefix** that precedes the shell invocation:

```rust
// exec/mod.rs — sketch
fn build_command(prefix: &[OsString], command: &str) -> tokio::process::Command {
    #[cfg(unix)]
    {
        let mut c = match prefix.split_first() {
            Some((program, rest)) => {
                let mut c = tokio::process::Command::new(program);
                c.args(rest).arg("sh").arg("-c").arg(command);
                c
            }
            None => {
                let mut c = tokio::process::Command::new("sh");
                c.arg("-c").arg(command);
                c
            }
        };
        c
    }
    #[cfg(windows)] { /* unchanged: cmd /C <command>; prefix is always empty */ }
}
```

`spawn_capped` gains a `prefix: &[OsString]` parameter threaded into `build_command`.

- **`HostBackend` and the Linux `OsSandboxBackend`** pass an **empty** prefix →
  `sh -c <command>` exactly as today (no behavioural change).
- **macOS Seatbelt** passes a precomputed prefix
  `["sandbox-exec", "-D", "ROOT=<canonical-root>", "-p", "<profile>"]` →
  `sandbox-exec -D ROOT=<root> -p <profile> sh -c <command>`.

**Rejected alternatives:** (a) pass a `make_command: impl FnOnce(&str) -> Command`
closure — more flexible but more churn at every call site for no benefit here; (b)
wrap the whole thing as one outer `sh -c 'sandbox-exec … sh -c …'` string — quoting
hell with arbitrary user commands and multi-line profiles. The prefix approach is the
minimal, readable change.

**Cross-platform compile guard:** because this edits Linux-compiled call sites, it
must compile on both targets — verified by Linux CI **and** locally via
`cargo check --target x86_64-unknown-linux-gnu --features os-sandbox` (lib only;
`landlock`/`seccompiler` are pure Rust so the cross-check needs no C toolchain).

## 6. The Seatbelt backend

### 6.1 Builder (method-for-method parity with the Linux builder)

```rust
// #[cfg(all(feature = "os-sandbox", target_os = "macos"))]
OsSandboxBackend::builder(sandbox)
    .timeout(Duration)            // default 30s
    .env_allowlist([..])          // default ["PATH","HOME"]; replaces inherited env
    .max_output_bytes(..)         // default 1 MiB
    .rlimits(ResourceLimits { .. })
    .allow_network(false)         // default DENY; all-or-nothing
    .read_paths([..])             // extra read-only (subpath …) exceptions
    .build() -> Result<Arc<dyn ExecutionBackend>, OsSandboxError>
```

Identical signatures to the Linux builder, so
`OsSandboxBackend::builder(sandbox).allow_network(false).build()` compiles unchanged
on both OSes. macOS interpretations:

- `read_paths` → extra `(allow file-read* (subpath "…"))` entries in the profile.
- `rlimits` → applied via `Command::pre_exec` + `libc::setrlimit` (reusing the shared
  `apply_rlimits`). `setrlimit` is inherited across the `sandbox-exec` → `sh` exec
  chain, so the limits reach the command. `sandbox-exec` itself uses negligible CPU,
  so an `RLIMIT_CPU` backstop is not tripped by the wrapper.
- `allow_network` → adds `(allow network*)` to the profile when `true`.

### 6.2 Profile generation (injection-safe, canonicalized)

The SBPL profile is **static per backend** (root + `allow_network` fixed at `build()`),
so the full `sandbox-exec` argv prefix is **precomputed once in `build()`** and stored
on the backend.

- **Root passed as a parameter, not interpolated.** `sandbox-exec -D ROOT=<root>` with
  the profile referencing `(subpath (param "ROOT"))`. The path is never string-spliced
  into the profile text, so there is **no SBPL escaping/injection risk**.
- **Canonicalize the root** in `build()` before use: macOS temp dirs resolve
  `/var/folders/…` → `/private/var/folders/…`, and Seatbelt matches **resolved** paths.
  The same canonical root is set as the child's `current_dir` (via `ExecConfig.cwd`),
  so the shell starts inside the write-allowed subpath and relative writes land there.

Profile (strict read-deny parity — §2.2):

```scheme
(version 1)
(deny default)
(allow process*)
(allow sysctl-read)
(allow mach-lookup)
(allow file-read*
  (subpath "/usr") (subpath "/bin") (subpath "/sbin")
  (subpath "/System") (subpath "/Library")
  (subpath "/private/var/db/dyld")
  (subpath "/etc") (subpath "/dev"))
(allow file-read* file-write*
  (subpath (param "ROOT")))
(allow file-write*
  (literal "/dev/null") (literal "/dev/stdout") (literal "/dev/stderr"))
; when allow_network(true):  (allow network*)
```

The exact system-read set is finalized in the plan against the real macOS runner /
dev machine (the read-deny fragility risk of §2.2 lives here). `read_paths` entries
append further `(allow file-read* (subpath …))` lines.

### 6.3 Fail-closed construction

`build()` returns `Err(OsSandboxError::Unsupported)` if Seatbelt cannot be applied —
**never** a silent downgrade below what `guarantees()` advertises. The probe runs the
**real profile** against a trivial binary:

```
sandbox-exec -D ROOT=<canonical-root> -p <profile> /usr/bin/true
```

A `NotFound` (the `sandbox-exec` binary is absent) or a non-zero exit ⇒ `Err`. This
validates **both** binary presence **and** that the profile compiles and a process can
actually start under it (it doubles as a profile smoke-test, since `/usr/bin/true`
must read the dyld cache + its own image under the allowed read set). `build()` stays
**synchronous** (parity with the Linux backend); the probe is a one-time
`std::process::Command` call.

`OsSandboxError` is a crate-local `thiserror` / `#[non_exhaustive]` enum parallel to
the Linux `OsSandboxError` (a single `Unsupported(String)` variant suffices). **No
`paigasus-helikon-core` change** — so no 5-step ascend, no manual facade bump.

### 6.4 `run` and `guarantees`

`run` calls the shared `spawn_capped` with the stored Seatbelt prefix and a `pre_exec`
that applies only `apply_rlimits` (the jail itself is applied by `sandbox-exec`, not by
a `pre_exec` hook — the whole point of Option A). `guarantees()`:

```rust
SandboxGuarantees {
    filesystem: Isolation::OsKernel,
    network:    if self.allow_network { Isolation::None } else { Isolation::OsKernel },
    syscalls:   Isolation::None,            // §2.3 — Seatbelt is not a syscall filter
    label:      "os-sandbox (seatbelt)",
}
```

## 7. Error model (unchanged from SMA-413 §9)

| Condition | Type |
|-----------|------|
| Bash args fail schema/deserialize | `ToolError::InvalidArgs` |
| Command blocked by allow/deny list | `ToolError::Denied { reason }` |
| Shell spawn / I/O failure during `run` | `ToolError::Other(anyhow)` |
| Non-zero exit / timed-out / truncated | **not errors** — fields on `ExecOutput` |
| `OsSandboxBackend` can't establish isolation | `OsSandboxError` (construction) |

A sandbox-denied operation (write outside root, denied `connect`) surfaces as a
**normal non-zero `ExecOutput`** with the OS error text on stderr — not a `ToolError`,
exactly like the Linux backend.

## 8. Dependencies & release mechanics

- **Dependencies: none added.** `sandbox-exec` (OS binary), `std::process`, and the
  existing `[target.'cfg(unix)'.dependencies] libc` cover everything. `cargo deny`
  has nothing new to check.
- **Feature:** no change — the existing
  `os-sandbox = ["dep:landlock", "dep:seccompiler"]` declaration already enables this
  path on macOS (the `dep:` deps stay target-gated out on non-Linux). The macOS
  backend is gated purely by `target_os = "macos"`, not by an extra feature.
- **Facade:** no `Cargo.toml` change — `tools-os-sandbox = ["tools",
  "paigasus-helikon-tools/os-sandbox"]` already forwards the feature, which now also
  reaches the macOS path. (A facade README/docs note that os-sandbox now covers macOS.)
- **Release:** purely additive ⇒ `feat(tools): SMA-426 add macOS Seatbelt
  ExecutionBackend` (not `feat(tools)!:` — no breaking change). release-plz selects a
  **patch** bump on 0.x for an additive feat → `0.2.1 → 0.2.2`. **No** core bump, **no**
  ascend ritual, **no** manual facade bump (this is a pure-auto consumer; a manual
  bump would defeat the cascade). Commit the `Cargo.lock` update if any (none expected,
  since no deps change).

## 9. Testing & the demo

New `tests/os_sandbox_seatbelt.rs`, gated
`#![cfg(all(feature = "os-sandbox", target_os = "macos"))]`, mirroring
`tests/os_sandbox.rs`. A `seatbelt_unavailable()` guard calls `build()` and on `Err`
does a loud `eprintln!("SKIP: …")` + `return` (never silently green — §2.4):

- **`builds_and_reports_guarantees`** — asserts `filesystem: OsKernel`,
  `network: OsKernel` (default deny), `syscalls: None`, `label: "os-sandbox (seatbelt)"`.
- **`blocks_write_outside_root_at_os_layer`** — `echo pwned > <sibling-dir>/escape.txt`
  exits non-zero and **creates no file**, while `echo ok > inside.txt` under the root
  succeeds. The outside path is one the shell's own logic would allow; Seatbelt blocks
  it at the OS layer.
- **`denies_network_by_default`** / **`allows_network_when_opted_in`** — a `connect()`
  attempt to a local address. **Distinguish the two error modes:** under default-deny
  Seatbelt blocks `connect` with **EPERM** ("Operation not permitted"); under
  `allow_network(true)` the connect reaches the stack and a closed local port yields
  **ECONNREFUSED** ("Connection refused"). Assert on the stderr signature so neither
  direction passes vacuously. (Note: Seatbelt enforces at `connect`, not socket
  creation, so the Linux test's "create an `AF_INET` socket" check does **not** port
  directly.) The exact probe tool — likely `/usr/bin/nc` (lives under the allowed
  `/usr/bin`) — is finalized against the real runner; the macOS dev machine makes this
  directly verifiable.

**CI:** these run on the macOS test job via `cargo test --workspace --all-features`
(macOS jobs are signal-only, not required checks, but they **do** run — satisfying the
AC that "Seatbelt tests run on the macOS runner"). Planning confirms the runner
enforces Seatbelt so the tests pass-because-enforced, not skip.

**Compile-only-on-macOS caveat:** the `clippy`, `docs`, and `doc-coverage` CI jobs run
on ubuntu, where this module is cfg'd out — they will **not** lint, doc-check, or
coverage-gate the macOS code. Mitigation: the dev machine is macOS, so local
`cargo clippy --all-features --all-targets` and `cargo doc --all-features` cover it; the
macOS test job compiles it. (Symmetric to the Linux backend, which the macOS dev
machine doesn't compile — hence the §5 cross-check.)

**Example:** extend `examples/os_sandbox_demo.rs`'s cfg to include `target_os = "macos"`
(same `guarantees()` print + blocked-write attempt), so
`cargo run -p paigasus-helikon-tools --features os-sandbox --example os_sandbox_demo`
works on macOS.

## 10. Docs (same PR, per CLAUDE.md)

- **mdBook** tools/sandbox page: add macOS Seatbelt to the backend matrix and the
  platform table (now "Linux: Landlock + seccomp" **or** "macOS: Seatbelt"); document
  the exact posture — strict read-deny, write-only-root, `network` all-or-nothing,
  `syscalls: None`, the `sandbox-exec` mechanism + Apple-deprecation caveat — so the
  `OsKernel` axes are never oversold. `mdbook build docs/book` stays clean
  (`warning-policy = "error"`).
- **Crate `README.md`** (`crates/paigasus-helikon-tools/README.md`): update the
  `os-sandbox` feature story to state Linux **and** macOS support and the per-OS
  mechanism.
- **Facade `README.md`** + a doc note: `tools-os-sandbox` now covers macOS too.
- **CHANGELOG** for `-tools`.
- Crate-level + `///` docs on every new `pub` item (write them even though ubuntu
  doc-coverage won't gate the macOS-cfg'd items).

## 11. Acceptance criteria (restated against this design)

- On macOS, a command writing outside the sandbox root is blocked **at the OS layer**
  (Seatbelt `(deny default)` + write-only-root), not just by path validation; writing
  inside the root succeeds. (§6.2, §9)
- With network denied, outbound network from the sandboxed command fails (`connect`
  EPERM under `(deny default)`). (§9)
- `OsSandboxBackend` builds and runs on macOS with **no** change to `BashTool` or agent
  code vs. the Linux backend (same type names + builder signatures); `guarantees()`
  accurately reflects the Seatbelt tier (`filesystem`/`network` `OsKernel`,
  `syscalls` `None`, label `"os-sandbox (seatbelt)"`). (§4, §6.1, §6.4)
- CI honesty: Seatbelt tests run on the macOS runner; where they cannot enforce they
  skip with a loud reason — never silently green. (§9)
- Fail-closed construction: if the profile cannot be applied, `build()` errors rather
  than silently downgrading. (§6.3)
- All CI gates green (fmt, clippy, the test matrix incl. the macOS Seatbelt tests,
  docs, doc-coverage, commits, pr-title, audit, deny), and `mdbook build` clean.

## 12. Out of scope (YAGNI)

- Domain-level network egress proxy — the shared follow-up (SMA-413 §11), promotes the
  SMA-412 web policy to a public type + a CONNECT proxy. macOS network stays
  all-or-nothing here.
- Any `BashTool` / `ExecutionBackend` trait change — this slots into the existing seam.
- The `sandbox_init` FFI backend (Option B) — the trait seam keeps it a future swap.
- microVM / Firecracker (SMA-416), container (Docker) backends.
