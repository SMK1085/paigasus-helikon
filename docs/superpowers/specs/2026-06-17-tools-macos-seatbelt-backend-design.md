# SMA-426 — `paigasus-helikon-tools`: macOS Seatbelt `ExecutionBackend` for Bash

**Status:** approved (brainstorm) — revised post-staff-review + live spike
**Ticket:** [SMA-426](https://linear.app/smaschek/issue/SMA-426/paigasus-helikon-tools-macos-seatbelt-executionbackend-for-bash)
**Milestone:** Composition & Extensibility
**Branch:** `feature/sma-426-paigasus-helikon-tools-macos-seatbelt-executionbackend-for`
**Date:** 2026-06-17
**Builds on:** [SMA-413 design](2026-06-16-tools-execution-backend-design.md) (the `ExecutionBackend` trait + Linux Landlock/seccomp backend) — see its §8 and §11.
**Reviewed by:** [staff review](2026-06-17-tools-macos-seatbelt-backend-design-review.md) (all 13 points dispositioned in §13).

## 1. Summary

SMA-413 shipped the object-safe `ExecutionBackend` trait plus an `OsSandboxBackend`
enforcing filesystem + syscall + network containment **on Linux only** (`landlock` +
`seccompiler`). It deferred macOS because no maintained, permissively-licensed,
pure-Rust Seatbelt binding exists — the one crate bundling both (`birdcage`) is GPL-3.0,
failing our `Apache-2.0 OR MIT` license + the `cargo deny` gate.

This ticket adds the **macOS** containment path behind the **same trait**, with the
**same public type names** (`OsSandboxBackend`, `OsSandboxBackendBuilder`,
`OsSandboxError`), cfg'd so exactly one backend compiles per target. Containment is
enforced by **Seatbelt** via the `sandbox-exec` binary. Swapping the backend needs
**no change to `BashTool` or agent code** — the headline AC.

Two properties make this lighter than the Linux side: **zero new crate dependencies**
(`sandbox-exec` is an OS binary; the profile is a string; `rlimit`s use `libc`, already
a unix dep) — which sidesteps the license/`cargo deny` problem entirely — and it is
**natively testable on the dev machine** (macOS).

`-tools` is at `0.2.1`; this is purely **additive** (new platform path, same type
names) → **patch** bump `0.2.2` via release-plz's normal flow. **No
`paigasus-helikon-core` change.**

## 2. Decisions (brainstorm + post-review)

1. **Mechanism: `sandbox-exec` wrapper (Option A), not `sandbox_init` FFI (Option B).**
   Run the command as `/usr/bin/sandbox-exec -D ROOT=<root> -p <profile> sh -c <command>`.
   The OS binary compiles + applies the SBPL profile in its own fresh process before
   exec'ing the shell. Option B is rejected: `sandbox_init` compiles the profile (it
   allocates) → **not async-signal-safe**, so calling it in a post-`fork` `pre_exec`
   hook inside multithreaded Tokio is a deadlock hazard; the compile-in-parent /
   apply-in-child split exists only as **private Apple SPI**. `sandbox-exec` avoids
   both, adds no deps, and the trait seam keeps FFI swappable later. `sandbox-exec` is
   Apple-deprecated but ships on every macOS and is widely used (e.g. Nix); the caveat
   is documented.

2. **Filesystem posture: WRITE-FOCUSED (revised from strict read-deny after the §3
   spike).** `(deny default)`; **reads allowed broadly** (`(allow file-read*)`);
   **read+write only within the sandbox root**; `/dev/null`-style write literals.
   Strict read-deny parity with the Linux Landlock rules was the original plan but is
   **empirically infeasible** via `sandbox-exec` on Cryptex-era macOS (§3): restricting
   reads breaks dyld before the shell can run, with no diagnostics. The write-outside-
   root and network ACs are fully met; the read-restriction half is withdrawn. This is
   **weaker than the Linux read+write posture** and is documented as such — never
   oversold (§7, §11).

3. **`guarantees()`: `filesystem: OsKernel`, `network: OsKernel`/`None`,
   `syscalls: None`, `label: "os-sandbox (seatbelt)"`.** `filesystem: OsKernel` is
   honest — a kernel mechanism *does* enforce write containment — but the shared
   `Isolation` enum (`None | OsKernel`) cannot encode "writes contained, reads open,"
   so the **label + backend docs carry that nuance explicitly** (the staff review's
   point #4). We deliberately do **not** add a new `Isolation` variant for one
   platform's nuance in v1 (YAGNI; the enum is `#[non_exhaustive]` so a finer tier
   stays additive later). `syscalls: None` because Seatbelt is an operation-based MAC,
   not a syscall filter, and our profile allows `process*` so the syscall surface is
   unconstrained.

4. **CI honesty: runtime guard + `HELIKON_REQUIRE_SANDBOX=1` hard-fail + macOS as a
   required check.** Tests **run** on the macOS runner; a `seatbelt_unavailable()`
   guard skips with a loud `eprintln!` only when the sandbox is genuinely unavailable.
   In CI we set `HELIKON_REQUIRE_SANDBOX=1`, which flips that skip into a **hard
   failure** — so a runner that silently stops enforcing turns the build red instead of
   green (the review's #2; retrofit to the Linux test too). `test (macos-latest,
   stable)` is promoted to a **required** status check (review #1) so the macOS-only
   backend cannot merge behind green required checks with a compile/clippy/sandbox
   break. The ticket AC was amended to match (review #3).

## 3. Empirical validation (live `sandbox-exec` spike, macOS 26.5.1 / arm64)

Run during the brainstorm to de-risk the highest-uncertainty item (the staff review's
#6). Results that shaped the design:

| Probe | Result |
|-------|--------|
| `(deny default)(allow file-read* process*)` — **read-anything** | ✅ shell runs (exit 0) |
| **Write-focused** (read-all, write→**canonical** root), write inside root | ✅ succeeds |
| Write-focused, write **outside** root | ✅ `Operation not permitted`, no file created |
| Strict read-deny: restrict reads to system subpaths incl. `/System`, explicit cryptex | ❌ **SIGABRT (134), no stderr** — dyld can't map the shared cache |
| Strict read-deny via Apple's `(allow file-read* dyld_subpaths)` macro | ❌ `unbound variable: dyld_subpaths` — **not exposed** to external profiles |
| Write inside root using the **raw** `/var/folders/…` path (un-canonicalized) | ❌ `Operation not permitted` — Seatbelt matches the resolved `/private/var/folders/…` |
| Network deny, `python3` connect probe | ✅ `EPERM` (`[Errno 1] Operation not permitted`) |
| Network allow (`(allow network*)`), same probe | ✅ `REFUSED` (`[Errno 61]`) — reached the stack |

**Facts established:**
- On macOS ≥ Big Sur (confirmed on 26.5.1) the dyld shared cache lives in the Preboot
  cryptex (`/System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld/`), a **separate
  volume** (distinct device id) from `/System`. Apple maps it inside their profiles via
  the built-in `dyld_subpaths` variable, which `sandbox-exec` does **not** expose to
  user profiles. There is no robust, documented, version-stable way to read-allow just
  the cache → **read-restriction is off the table for v1.**
- `sandbox-exec` is at **`/usr/bin/sandbox-exec`** (the review's #9 said `/usr/sbin` —
  wrong dir; the absolute-path principle is still adopted, §6.3/§5).
- `Sandbox::open` already canonicalizes (`sandbox.rs:35`) and `Sandbox::root()` returns
  the canonical path — so the backend **reuses `sandbox.root()`** and does **not**
  re-canonicalize (review #13). The spike proved the canonical form is *required*.
- The network test must distinguish `EPERM` (sandbox-blocked) from `ECONNREFUSED`
  (sandbox allowed, stack refused) via a `python3` probe that prints **our own
  markers**, not localized OS strings (review #11).

## 4. Module layout

```
crates/paigasus-helikon-tools/src/exec/
  mod.rs                  # generalize build_command (§5); wire the macOS cfg branch
  host.rs                 # unchanged behaviour (empty-prefix call site)
  os_sandbox.rs           # Linux Landlock+seccomp (unchanged except empty-prefix call site)
  os_sandbox_seatbelt.rs  # NEW — macOS Seatbelt backend + builder + OsSandboxError
```

`mod.rs` re-exports the **same names** as the Linux module, cfg'd per-OS so exactly one
compiles and consumer code is byte-identical across OSes:

```rust
#[cfg(all(feature = "os-sandbox", target_os = "macos"))]
mod os_sandbox_seatbelt;
#[cfg(all(feature = "os-sandbox", target_os = "macos"))]
pub use os_sandbox_seatbelt::{OsSandboxBackend, OsSandboxBackendBuilder, OsSandboxError};
```

(The Linux branch keeps its existing
`#[cfg(all(feature = "os-sandbox", target_os = "linux", any(x86_64, aarch64)))]` gate.)
Every new `pub` item carries a `///` doc.

## 5. The one shared-code change: generalize `build_command`

`spawn_capped` stays the orchestrator (cwd, env scrub, capped pipe drains, timeout,
process-group subtree kill, reaping — all inherited across the `sandbox-exec` → `sh`
exec chain). Only **how the base command is built** becomes pluggable, via an argv
**prefix** preceding the shell:

```rust
// exec/mod.rs — sketch (the real spawn_capped keeps its `configure_child` closure param)
fn build_command(prefix: &[std::ffi::OsString], command: &str) -> tokio::process::Command {
    #[cfg(unix)]
    {
        match prefix.split_first() {
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
        }
    }
    #[cfg(windows)]
    {
        // `_prefix` is unused on Windows (always empty) — avoid `unused_variables`
        // under -D warnings; clippy runs on ubuntu + Windows is signal-only.
        let _ = prefix;
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    }
}
```

`spawn_capped` gains a `prefix: &[OsString]` parameter threaded into `build_command`.
`HostBackend` and the Linux `OsSandboxBackend` pass an **empty** prefix (no behaviour
change). macOS passes a precomputed prefix
`["/usr/bin/sandbox-exec", "-D", "ROOT=<root>", "-p", "<profile>"]`.

**Rejected:** (a) a `make_command: impl FnOnce(&str) -> Command` closure — more churn at
every call site for no benefit; (b) one outer `sh -c 'sandbox-exec … sh -c …'` string —
quoting hell. The prefix is the minimal, readable change.

**Cross-platform compile guard:** this edits Linux-compiled call sites, so it must
compile on both targets — verified by Linux CI **and** locally via
`cargo check --target x86_64-unknown-linux-gnu --features os-sandbox` (lib only;
`landlock`/`seccompiler` are pure Rust, no C toolchain needed).

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
    .read_paths([..])             // see note
    .build() -> Result<Arc<dyn ExecutionBackend>, OsSandboxError>
```

Identical signatures to the Linux builder, so
`OsSandboxBackend::builder(sandbox).allow_network(false).build()` compiles unchanged on
both OSes. macOS interpretations:

- `rlimits` → applied via `Command::pre_exec` + the shared `apply_rlimits`
  (`#[cfg(unix)]`, async-signal-safe `setrlimit`), inherited across the exec chain.
- `allow_network` → adds `(allow network*)` to the profile when `true`.
- **`read_paths` → documented NO-OP on macOS.** Under the write-focused posture reads
  are already unrestricted (§2.2), so extra read-only exceptions are meaningless. The
  method stays for API parity (byte-identical consumer code) and is documented as
  having no effect on this backend. (This resolves review #5: no developer-supplied
  path is ever spliced into the profile, so there is no SBPL-injection surface — see
  §6.2.)

### 6.2 Profile generation (no path splicing)

The profile is **static per backend** (only `allow_network` varies at `build()`), so
the full `sandbox-exec` argv prefix is **precomputed once in `build()`** and stored.

- **Root passed as a parameter, never spliced.** `-D ROOT=<root>` with the profile
  referencing `(subpath (param "ROOT"))`. No path text enters the profile string, so
  there is **no SBPL escaping/injection risk** (and `read_paths` is a no-op, §6.1, so
  nothing else is spliced either).
- **Reuse `sandbox.root()`** as `ROOT` (already canonical — §3, review #13). No
  re-canonicalization. The same path is the child's `current_dir` (via `ExecConfig.cwd`).

Profile (write-focused — §2.2):

```scheme
(version 1)
(deny default)
(allow process*)
(allow sysctl-read)
(allow mach-lookup)
(allow file-read*)                       ; reads unrestricted (dyld needs the cryptex cache)
(allow file-write*
  (subpath (param "ROOT"))
  (literal "/dev/null") (literal "/dev/stdout") (literal "/dev/stderr")
  (literal "/dev/dtracehelper") (literal "/dev/tty"))
; when allow_network(true):  (allow network*)
```

**Residual-risk acknowledgement (review #8, #10), documented in the backend docs:**
- `(allow process*)` and `(allow mach-lookup)` are **unscoped** — broad `mach-lookup`
  lets the process reach any registered mach/XPC service (historically a Seatbelt-escape
  pivot), and `process*` leaves the syscall surface open (hence `syscalls: None`).
  Scoping `mach-lookup` to the specific services dyld/libsystem need is fragile and
  version-sensitive; **v1 documents the residual risk instead of attempting it.**
- Under deny-network, Seatbelt also blocks **`AF_UNIX` local IPC** (`network*` covers
  it), which is **stricter** than the Linux backend (Linux leaves `AF_UNIX` allowed).
  Documented as a per-platform difference; acceptable (a deny-network shell rarely needs
  local IPC, and our spike's `sh`/coreutils/`python3` ran fine without it).

### 6.3 Fail-closed construction (probe)

`build()` returns `Err(OsSandboxError::Unsupported)` if Seatbelt cannot be applied —
never a silent downgrade. The probe exercises a **real shell that writes inside the
root and is blocked outside it**, so a profile that compiles but doesn't actually
contain is caught at construction (review #7 — `/usr/bin/true` was too weak):

```
/usr/bin/sandbox-exec -D ROOT=<root> -p <profile> /bin/sh -c \
  'echo ok > "<root>/.helikon-probe" && ! (echo x > /helikon-probe-denied) '
```

(Conceptually: assert the inside-root write succeeds **and** an outside-root write is
denied; clean up the probe file.) A `NotFound` (binary absent), a non-zero exit, or the
outside-write unexpectedly succeeding ⇒ `Err`. `build()` stays **synchronous** (parity
with Linux); the probe is a one-time `std::process::Command`. `OsSandboxError` is a
crate-local `thiserror` / `#[non_exhaustive]` enum with a single `Unsupported(String)`
variant. **No `paigasus-helikon-core` change.**

### 6.4 `run` and `guarantees`

`run` calls the shared `spawn_capped` with the stored Seatbelt prefix and a `pre_exec`
applying only `apply_rlimits` (the jail itself is applied by `sandbox-exec`). A
sandbox-denied operation surfaces as a **normal non-zero `ExecOutput`** with the OS
error text on stderr — not a `ToolError`, exactly like the Linux backend.

```rust
fn guarantees(&self) -> SandboxGuarantees {
    SandboxGuarantees {
        filesystem: Isolation::OsKernel,                                   // write-containment (docs clarify)
        network:    if self.allow_network { Isolation::None } else { Isolation::OsKernel },
        syscalls:   Isolation::None,                                       // §2.3 — not a syscall filter
        label:      "os-sandbox (seatbelt)",
    }
}
```

## 7. Error model (unchanged from SMA-413 §9)

| Condition | Type |
|-----------|------|
| Bash args fail schema/deserialize | `ToolError::InvalidArgs` |
| Command blocked by allow/deny list | `ToolError::Denied { reason }` |
| Shell spawn / I/O failure during `run` | `ToolError::Other(anyhow)` |
| Non-zero exit / timed-out / truncated / **sandbox-denied op** | **not errors** — fields on `ExecOutput` |
| `OsSandboxBackend` can't establish isolation | `OsSandboxError` (construction) |

## 8. Dependencies & release mechanics

- **Dependencies: none added.** `sandbox-exec` (OS binary), `std::process`, existing
  `libc`. `cargo deny` has nothing new to check.
- **Feature:** no change — the existing
  `os-sandbox = ["dep:landlock", "dep:seccompiler"]` already enables this path on macOS
  (the `dep:` deps stay target-gated out on non-Linux). The macOS backend is gated by
  `target_os = "macos"`, not by an extra feature.
- **Facade:** no `Cargo.toml` change — `tools-os-sandbox` already forwards the feature
  (now reaching the macOS path). A facade README/docs note that os-sandbox now covers
  macOS.
- **Release:** purely additive ⇒ `feat(tools): SMA-426 add macOS Seatbelt
  ExecutionBackend` (not `feat(tools)!:`). release-plz selects a **patch** bump on 0.x
  for an additive feat → `0.2.1 → 0.2.2`. **No** core bump, **no** ascend ritual, **no**
  manual facade bump (pure-auto consumer).

## 9. Testing & the demo

New `tests/os_sandbox_seatbelt.rs`, gated
`#![cfg(all(feature = "os-sandbox", target_os = "macos"))]`, mirroring
`tests/os_sandbox.rs`. A shared guard governs CI honesty (§2.4):

```rust
/// Returns true (caller should `return`) if Seatbelt can't be built here.
/// If HELIKON_REQUIRE_SANDBOX=1, an unavailable sandbox is a hard FAILURE,
/// not a skip — so a CI runner that stops enforcing turns the build red.
fn seatbelt_unavailable(root: &std::path::Path) -> bool {
    if OsSandboxBackend::builder(Sandbox::open(root).unwrap()).build().is_ok() {
        return false;
    }
    if std::env::var("HELIKON_REQUIRE_SANDBOX").as_deref() == Ok("1") {
        panic!("HELIKON_REQUIRE_SANDBOX=1 but Seatbelt could not be established");
    }
    eprintln!("SKIP: Seatbelt unavailable on this host; os-sandbox AC not exercised");
    true
}
```

Tests:
- **`builds_and_reports_guarantees`** — `filesystem: OsKernel`, `network: OsKernel`
  (default deny), `syscalls: None`, `label: "os-sandbox (seatbelt)"`.
- **`blocks_write_outside_root_at_os_layer`** — `echo pwned > <sibling-dir>/escape.txt`
  exits non-zero and **creates no file**; `echo ok > inside.txt` under the root
  succeeds.
- **`denies_network_by_default`** / **`allows_network_when_opted_in`** — a `python3`
  connect probe to `127.0.0.1:9` that prints **our own markers**: assert `EPERM` under
  default-deny and `REFUSED` (reached the stack) under `allow_network(true)`. Assert on
  those markers + exit, **not** localized OS strings (review #11). Validated working in
  the §3 spike.

**Retrofit:** add the same `HELIKON_REQUIRE_SANDBOX` hard-fail to the Linux
`landlock_unavailable()` guard in `tests/os_sandbox.rs` (review #2 applies to both
backends).

**CI changes:**
- `.github/workflows/ci.yml`: set `HELIKON_REQUIRE_SANDBOX=1` for the test job
  (at least on the macOS + ubuntu runners where enforcement is expected).
- `.github/rulesets/main-protection-checks.json`: add `test (macos-latest, stable)` to
  the required contexts (review #1); update CONTRIBUTING.md's required-checks list to
  match.

**Compile-only-on-macOS caveat:** `clippy`, `docs`, `doc-coverage` run on ubuntu where
this module is cfg'd out, so they won't lint/doc-check/coverage-gate it. Mitigations:
the dev machine is macOS (local `cargo clippy --all-features --all-targets` +
`cargo doc --all-features` cover it) **and** macOS test is now a required check (so a
compile/clippy break there blocks merge).

**Example:** extend `examples/os_sandbox_demo.rs`'s cfg to include `target_os = "macos"`
(same `guarantees()` print + blocked-write attempt).

## 10. Docs (same PR, per CLAUDE.md)

- **mdBook** tools/sandbox page: add macOS Seatbelt to the backend matrix + platform
  table; state the posture honestly — **write-containment only (reads unrestricted)**,
  `network` all-or-nothing (also blocks `AF_UNIX` when denied), `syscalls: None`, the
  `sandbox-exec` mechanism + Apple-deprecation caveat, and the unscoped
  `mach-lookup`/`process*` residual risk (§6.2). Make explicit that macOS `OsKernel` is
  **weaker** than Linux `OsKernel` on the filesystem axis so the label isn't read as
  cross-platform-equivalent. `mdbook build docs/book` stays clean.
- **Crate `README.md`**: `os-sandbox` now Linux **and** macOS, with the per-OS
  mechanism + per-OS posture.
- **Facade `README.md`** + note: `tools-os-sandbox` covers macOS too.
- **CHANGELOG** for `-tools`. Crate-level + `///` docs on every new `pub` item.

## 11. Acceptance criteria (restated against this design)

- On macOS, a command writing outside the sandbox root is blocked **at the OS layer**
  (Seatbelt `(deny default)` + write-only-root); writing inside the root succeeds. (§3, §6.2, §9)
- With network denied, outbound network fails (`connect` EPERM). (§3, §9)
- `OsSandboxBackend` builds and runs on macOS with **no** change to `BashTool`/agent
  code vs. the Linux backend (same type names + builder signatures); `guarantees()`
  reflects the Seatbelt tier honestly — `filesystem`/`network` `OsKernel`, `syscalls`
  `None`, label `"os-sandbox (seatbelt)"`, with docs stating the filesystem axis is
  **write**-containment only. (§4, §6.1, §6.4, §10)
- CI honesty: Seatbelt tests **run** on the (now-required) macOS runner; they skip with
  a loud reason only when the sandbox is genuinely unavailable, and
  `HELIKON_REQUIRE_SANDBOX=1` makes that a hard failure in CI. (§2.4, §9)
- Fail-closed: if the profile can't be applied (or doesn't actually contain), `build()`
  errors. (§6.3)
- All CI gates green (incl. the now-required macOS test job), `mdbook build` clean.

## 12. Out of scope (YAGNI)

- Domain-level network egress proxy — shared follow-up (SMA-413 §11). macOS network
  stays all-or-nothing here.
- Read-restriction on macOS (infeasible via `sandbox-exec` — §3); revisit only if a
  permissive `sandbox_init`-class binding or a stable cryptex-read mechanism appears.
- A new `Isolation` variant for write-only containment (§2.3 — documented via label
  instead for v1).
- Any `BashTool`/trait change; the `sandbox_init` FFI backend; microVM/container backends.

## 13. Staff-review dispositions

| # | Severity | Disposition |
|---|----------|-------------|
| 1 | critical | **Adopted** — `test (macos-latest, stable)` promoted to a required check (§9). |
| 2 | critical | **Adopted** — `HELIKON_REQUIRE_SANDBOX=1` flips skip→hard-fail; retrofit to Linux (§2.4, §9). |
| 3 | critical | **Adopted** — ticket AC amended to the runtime-guard approach (Linear, 2026-06-17). |
| 4 | moderate | **Addressed** — write-focused `filesystem: OsKernel` documented via label/docs; no new enum variant in v1 (§2.3). |
| 5 | moderate | **Resolved by pivot** — `read_paths` is a no-op, ROOT via `-D`; nothing is spliced (§6.1–6.2). |
| 6 | moderate | **Confirmed fatal** — drove the write-focused pivot; spike evidence in §3. |
| 7 | moderate | **Adopted** — probe uses `/bin/sh` + inside/outside write checks (§6.3). |
| 8 | moderate | **Documented residual risk**; pushed back on scoping `mach-lookup` (fragile/YAGNI) (§6.2, §10). |
| 9 | minor | **Adopted with correction** — absolute path is `/usr/bin/sandbox-exec` (not `/usr/sbin`) (§3, §5). |
| 10 | minor | **Documented** — deny-network also blocks `AF_UNIX` (stricter than Linux) (§6.2, §10). |
| 11 | minor | **Resolved** — `python3` probe prints our own `EPERM`/`REFUSED` markers (§3, §9). |
| 12 | minor | **Adopted** — Windows `_prefix`; sketch keeps `configure_child` (§5). |
| 13 | minor | **Adopted** — reuse `sandbox.root()` (already canonical); no re-canonicalize (§3, §6.2). |
