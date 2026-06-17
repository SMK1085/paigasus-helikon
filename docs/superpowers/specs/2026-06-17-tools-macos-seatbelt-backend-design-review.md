# Staff review — SMA-426 macOS Seatbelt `ExecutionBackend` design

**Reviews:** [`2026-06-17-tools-macos-seatbelt-backend-design.md`](2026-06-17-tools-macos-seatbelt-backend-design.md)
**Ticket:** [SMA-426](https://linear.app/smaschek/issue/SMA-426/paigasus-helikon-tools-macos-seatbelt-executionbackend-for-bash)
**Reviewed against:** SMA-413 predecessor design, Linear SMA-426 + SMA-413, Notion Crate Reference, and the on-disk `crates/paigasus-helikon-tools/src/exec/` tree.
**Date:** 2026-06-17

## Verdict

The spec is well-reasoned and mostly faithful to the plan. The trait-seam framing, the
fail-closed posture, the "don't oversell `guarantees()`" discipline, and the
zero-new-deps / license argument are all sound and consistent with SMA-413. Several
issues will bite later, ranked below by severity. The two I'd treat as blockers are
**#1+#2** (no required CI gate, plus tests that pass green whether or not Seatbelt
enforces) and **#6** (validate the dyld read-set on the real runner before committing to
read-deny). Everything else is fixable in the plan or in review.

## Critical

### 1. The security-critical code has essentially zero gating CI coverage

Per `CLAUDE.md`, the only required test check is `test (ubuntu-latest, stable)`; the
macOS jobs are signal-only. The Seatbelt module is `#[cfg(target_os = "macos")]`, so on
ubuntu it is **not compiled at all** — `clippy`, `docs`, and `doc-coverage` all run on
ubuntu and skip it (the spec admits this in §9's "compile-only-on-macOS caveat"). So the
entire enforcement backend — a compile error, a clippy failure, or a broken sandbox
profile — can merge with every *required* check green. The only job that even compiles it
is non-required. The spec's mitigation is "the dev machine is macOS" — a human process,
not a gate.

**Recommendation:** make `test (macos-latest, stable)` a required status check (at least
scoped to this crate), or accept that the safety guarantee rides entirely on reviewer
discipline.

### 2. The skip pattern means the sandbox tests pass green whether or not Seatbelt enforces

§2.4 / §9 use a runtime `seatbelt_unavailable()` guard that does `eprintln!("SKIP")` +
`return`, mirroring the existing Linux `landlock_unavailable()` (confirmed in
`tests/os_sandbox.rs`). But `eprintln` in a passing test is invisible in CI summaries —
cargo reports `ok`. Combined with #1, a future runner image that silently stops enforcing
Seatbelt yields all-green with no enforcement. The spec leans on "planning confirms the
runner enforces Seatbelt," but nothing keeps that true over time. This is precisely the
"a sandbox test that passes because the sandbox is inactive is worse than no test"
anti-pattern the spec says it avoids.

**Recommendation:** set `HELIKON_REQUIRE_SANDBOX=1` in CI and have the guard convert
skip → hard-fail when it's set, so a runner that stops enforcing turns the build red. Worth
retrofitting onto the Linux test too.

### 3. This also misses the written acceptance criterion

SMA-426's AC literally says tests are "`#[ignore]`'d with an explicit reason where they
can't enforce." The spec consciously substitutes a runtime guard and argues it's satisfied
"in spirit." That's a defensible call, but it's an undocumented deviation from the ticket.

**Recommendation:** amend the ticket AC so it isn't flagged as unmet at close.

## Moderate

### 4. The documented read-deny fallback can't be represented honestly by `guarantees()`

§2.2's contingency is to fall back to read-allow-all / write-only-root and "honestly
document `filesystem: OsKernel` as write-containment." But `Isolation` is only
`None | OsKernel` (confirmed in `exec/mod.rs`). A consumer reading
`guarantees().filesystem == OsKernel` cannot distinguish "reads denied + writes contained"
from "reads wide open, only writes contained" — and the strict Linux backend reports the
identical value. The spec's own "don't oversell" principle breaks here: the enum is too
coarse to encode the fallback, so the fallback would ship reporting the same tier as full
containment.

**Recommendation:** either add a distinct `Isolation` value for the weaker posture
(SMA-413 §5 already notes new tiers are additive / non-breaking), or call this out as a
blocker on taking the fallback at all.

### 5. The "no SBPL injection risk" claim is contradicted by `read_paths`

§6.2 correctly passes `ROOT` via `-D ROOT=` so it's never spliced into profile text —
good. But the same section appends `read_paths` as literal
`(allow file-read* (subpath "…"))` lines, i.e. **string-spliced into the profile**. macOS
filenames can legally contain `"`, `)`, and newlines, so a read-path with those breaks
profile compilation (→ fail-closed `Err`) or injects SBPL. These are developer-supplied,
not model-supplied, so severity is lower — but the spec's absolute claim ("no SBPL
escaping/injection risk") is false as written.

**Recommendation:** pass each read path as its own `-D READn=…` parameter (the profile
still varies in line count, but no path text is spliced), or rigorously validate / reject
paths containing SBPL metacharacters.

### 6. The dyld-cache read path is the highest-risk deferred unknown, and it's hand-waved

§6.2 allows `(subpath "/private/var/db/dyld")` — the *pre-Big-Sur* shared-cache location.
On modern macOS (Ventura+, especially Apple Silicon) the dyld shared cache lives under the
Preboot cryptex (`/System/Volumes/Preboot/Cryptexes/...`), whose *resolved* path Seatbelt
may not treat as under `/System`. If the read set doesn't cover wherever the cache actually
resolves on the target OS/arch, then even the `/usr/bin/true` probe fails to map dyld →
`build()` always returns `Err` → the backend is unusable on both the dev machine and the
runner. The spec defers "the exact system-read set is finalized in the plan," but this
single line determines whether anything works at all. Dev-machine vs runner arch/OS drift
(e.g. arm64 Sequoia locally vs the GitHub image) can pass on one and fail the other, and
there's no test-matrix consideration for it.

**Recommendation:** validate the read-set empirically on the *exact* runner OS and arch
before committing to read-deny; pin the runner image; document the resolved cache path.

### 7. The probe (`/usr/bin/true`) is a weak proxy for "a shell works"

§6.3 validates the profile by running `/usr/bin/true`, but `true` loads far less than
`sh -c <cmd>` + coreutils (which touch `/etc`, `$HOME`, more dylibs, mach services). A
profile tight enough to fail real commands but pass `true` makes `build()` succeed while
every actual command fails at runtime with a cryptic sandbox-deny on stderr and a nonzero
exit — surfaced to the model as "your bash command mysteriously failed," not a clean
construction error. So the fail-closed guarantee is narrower than it reads.

**Recommendation:** probe with `/bin/sh -c 'echo ok'`, and ideally include a
write-inside-root plus a blocked-write-outside in the probe.

### 8. Unscoped `(allow mach-lookup)` / `(allow process*)` weaken the "strict parity" claim

Blanket `mach-lookup` lets the sandboxed process reach *any* registered mach / XPC service
— historically the pivot point for most published Seatbelt escapes — and `process*` leaves
the raw syscall surface open (the spec is honest that `syscalls: None`). So
`filesystem` / `network` report `OsKernel` identically to the much tighter Linux seccomp
posture, while the macOS jail is qualitatively weaker. That's an acceptable v1 tradeoff,
but it's unacknowledged, whereas the Linux design explicitly discussed targeted-deny vs.
allowlist.

**Recommendation:** scope `mach-lookup` to the specific services dyld / libsystem need, and
state the residual risk in the docs so "OsKernel" isn't read as cross-platform equivalent.

## Minor (worth fixing before code)

### 9. `sandbox-exec` should be invoked by absolute path

The prefix uses bare `"sandbox-exec"`, but `spawn_capped` does `env_clear()` then rebuilds
`PATH` from the allowlist. `sandbox-exec` lives in `/usr/sbin`, which is **not** in a
minimal `PATH` (e.g. a launchd / daemon context with `/usr/bin:/bin`). The `build()` probe
uses `std::process::Command` with the *full inherited* env, so it can find the binary while
the actual `env_clear`'d run gets `NotFound` — build passes, run fails. Use
`/usr/sbin/sandbox-exec`.

### 10. Network parity gaps `guarantees()` can't express

Linux denies at `socket(2)` (seccomp family filter, leaving `AF_UNIX` working); macOS
`(deny default)` denies at `network*` / `connect` and may also block `AF_UNIX` local IPC
unless carved out — yet both report `network: OsKernel`. §9's own tests acknowledge the
enforcement point differs (the Linux "create an `AF_INET` socket" test doesn't port).
Decide whether `allow_network(false)` should preserve local-IPC parity with Linux, and
document the per-platform meaning.

### 11. The network test asserts on localized OS error strings

Distinguishing EPERM ("Operation not permitted") from ECONNREFUSED ("Connection refused")
via `nc` / shell stderr text is brittle across macOS versions and locales (`nc`'s wording
varies). Prefer asserting exit code + a coarse errno marker from a tiny probe over matching
human-readable OS strings.

### 12. `build_command` sketch is slightly stale and risks a Windows lint break

The §5 sketch shows `build_command(prefix, command)` but omits that the real `spawn_capped`
already carries a `configure_child` closure (verified in `mod.rs`) — directionally fine,
just incomplete. More concretely, the new `prefix: &[OsString]` is unused on the
`#[cfg(windows)]` path → `unused_variables` under `-D warnings`. Clippy runs on ubuntu and
Windows is signal-only, so nobody catches it; name it `_prefix` or `#[allow]` it.

### 13. Redundant (harmless) canonicalization

§6.2 says to canonicalize the root in `build()`, but `Sandbox::open` already canonicalizes
and `Sandbox::root()` returns the canonical path (`/var/folders` → `/private/var/folders`
is already handled). The macOS backend should just reuse `sandbox.root()` like the Linux
one does — the spec's framing suggests this part of the integration surface wasn't fully
traced. Not a bug, just dead work.

## What the spec got right

- The zero-new-deps / license argument is solid and genuinely sidesteps the birdcage
  GPL-3.0 problem the Linux side wrestled with.
- `feat(tools)` → patch `0.2.2` is correct: `release-plz.toml` has no semver override, so
  0.x `feat` → patch and 0.x breaking → minor, matching SMA-413's `feat!` → 0.2.0. No core
  bump / no ascend ritual / no manual facade bump is the right read of `CLAUDE.md`.
- The process-group subtree-kill survives the `sandbox-exec` wrapper unchanged (the group
  is inherited across the exec chain).
- Rejecting Option B (`sandbox_init` in a post-fork `pre_exec`) on async-signal-safety
  grounds is exactly right.

## Suggested next actions

1. Resolve the CI-gating question (#1, #2, #3) with the repo owner — this is policy, not code.
2. Spike the read-set on the actual GitHub macOS runner image early (#6, #7) before writing the backend.
3. Fix `read_paths` injection (#5) and the `sandbox-exec` absolute path (#9) in the design before implementation.
4. Decide the fallback honesty story (#4) and the `mach-lookup` scoping (#8) and write the chosen posture into the spec.
