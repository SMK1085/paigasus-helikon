# HostBackend's default `env_allowlist` on Windows — design

**Ticket:** [SMA-614](https://linear.app/smaschek/issue/SMA-614/hostbackends-default-env-allowlist-is-unix-shaped-and-breaks-networked)
**Date:** 2026-09-05
**Status:** revised after adversarial challenge — awaiting approval

## Problem

`HostBackend::builder` defaults `env_allowlist` to `["PATH", "HOME"]`
(`crates/paigasus-helikon-tools/src/exec/host.rs:102`). `spawn_capped` calls
`.env_clear()` (`src/exec/mod.rs:219`) and then re-adds only allowlisted names, so
the allowlist is the *entire* environment the child sees.

`HOME` does not exist on Windows, so `std::env::var` returns `Err` and the entry is
silently a no-op. A default-configured `HostBackend` on Windows therefore hands the
child exactly one variable: `PATH`. That is a materially broken environment, and it
fails in ways that look like the *command* is wrong rather than the backend's
default.

### What is actually established, and what is hypothesis

This distinction is load-bearing for the test design, so it is stated up front.

**Established by reading the code:** `env_clear()` plus the allowlist is the whole
child environment; `HOME` is absent on Windows and its allowlist entry is a silent
no-op; therefore a default-configured Windows child sees `PATH` and nothing else.

**Hypothesis, never reproduced:** that `ping.exe` specifically fails Winsock
initialization and exits in milliseconds without `%SystemRoot%`. The SMA-569 design
doc
(`docs/superpowers/specs/2026-09-04-exec-timeout-exit-code-design.md:165`) hedges
this as "`ping.exe` **may** fail to initialize", and the workaround it added was
preventative — nobody has ever run `ping` on `windows-latest` *without*
`SystemRoot`. The first draft of this spec restated the hypothesis as fact. It is
not.

The consequence: **no test may depend on that hypothesis being true.** If it is
false, a `ping`-based assertion passes identically with and without the fix, and
acceptance criterion 1 would be proven by nothing. The test design below asserts the
environment directly instead.

## Scope

All four execution backends were checked.

| Backend | File | Gating | Has the bug? |
| --- | --- | --- | --- |
| `HostBackend` | `exec/host.rs` | none — every target | **Yes** |
| `OsSandboxBackend` (Landlock) | `exec/os_sandbox.rs` | `feature = "os-sandbox"` **and** `target_os = "linux"` **and** `x86_64`/`aarch64` | No — unreachable on Windows |
| `OsSandboxBackend` (Seatbelt) | `exec/os_sandbox_seatbelt.rs` | `feature = "os-sandbox"` **and** `target_os = "macos"` | No — unreachable on Windows |
| `ForkdBackend` | `exec/forkd.rs` | `feature = "microvm"` | No — has no env allowlist at all |

The two OS-sandbox backends carry the same `["PATH", "HOME"]` literal, which is
correct for their platforms; what they have is duplication, which this design
removes. `ForkdBackend` does not route through `ExecConfig`/`spawn_capped` and sends
no env by design — environment is a snapshot-boot concern for forkd, per
`docs/superpowers/specs/2026-06-21-sma-416-forkd-microvm-spike.md:204`. Nothing to
do there; recorded so the next reader does not have to re-derive it.

Out of scope: any change to what the unix default passes through.

## Design

### 1. One shared platform-aware const

In `src/exec/mod.rs`, beside the existing `DEFAULT_TIMEOUT` / `DEFAULT_MAX_OUTPUT`:

```rust
/// Environment variable names a child process receives when the caller does not
/// override the allowlist.
///
/// unix: `PATH`, `HOME`.
///
/// Windows: `PATH`, `SystemRoot`, `PATHEXT`, `TEMP`, `TMP`, `USERPROFILE`,
/// `APPDATA`, `LOCALAPPDATA`.
#[cfg(unix)]
pub const DEFAULT_ENV_ALLOWLIST: &[&str] = &["PATH", "HOME"];

/// See the unix arm for the full per-platform documentation.
#[cfg(windows)]
pub const DEFAULT_ENV_ALLOWLIST: &[&str] = &[
    "PATH", "SystemRoot", "PATHEXT", "TEMP", "TMP", "USERPROFILE", "APPDATA",
    "LOCALAPPDATA",
];
```

All four call sites — the three builders plus any future one — collect from it:

```rust
env_allowlist: DEFAULT_ENV_ALLOWLIST.iter().map(|s| (*s).to_owned()).collect(),
```

The explicit deref is required, not stylistic: `.iter()` over `&[&str]` yields
`&&str`, and `s.to_owned()` on a `&&str` resolves to `<&str as ToOwned>` and
produces a `&str`, which will not collect into `Vec<String>`. Preserve `(*s)`.

**No `cfg(not(any(unix, windows)))` arm.** `build_command` (`src/exec/mod.rs:292`)
already has only `#[cfg(unix)]` and `#[cfg(windows)]` bodies and would fail to
compile on any third target. An arm here would advertise portability the crate does
not have.

The `cfg` is inert in the two OS-sandbox builders — they are unix-gated, so they
always see `["PATH", "HOME"]` and their behaviour is byte-for-byte unchanged.

#### Inclusion principle

> **What a shell plus a config-reading CLI needs in order to start, excluding
> anything that can carry a credential or redirect execution.**

Every entry below is justified against that principle, and the principle also
generates the exclusions.

| Name | Why it is in the default |
| --- | --- |
| `PATH` | Lets `cmd.exe` resolve the programs named *inside* the command string. (It is not what locates `cmd.exe` itself — Rust's std resolves the program before applying the child environment.) |
| `SystemRoot` | Winsock provider DLL resolution and, more broadly, system DLL loading. |
| `PATHEXT` | `cmd.exe` extension resolution matches the machine's configuration rather than its built-in fallback. |
| `TEMP`, `TMP` | Windows has no hardcoded writable temp path the way unix has `/tmp`; `GetTempPath` degrades to the Windows directory, typically not writable. |
| `USERPROFILE` | Home-directory discovery for tools that read it. |
| `APPDATA`, `LOCALAPPDATA` | **The actual Windows analogue of what `HOME` buys on unix.** On unix, `HOME` is what gives a tool `~/.config` and `~/.cache`. On Windows, git, npm, Python/pip and the `dirs`/`directories` crates read `APPDATA`/`LOCALAPPDATA`, *not* `USERPROFILE`. Without these, the parity argument for `USERPROFILE` does not actually hold and `git`/`npm`/`python` still misbehave under the default. |

Deliberately excluded:

- **`COMSPEC`** — `cmd.exe` reads it to locate the program it spawns for pipes,
  `for /f`, and `start`, and it falls back to `%SystemRoot%\system32\cmd.exe`. By
  the first draft's own admission it was "belt-and-braces", i.e. no demonstrated
  benefit; and it is the one candidate that lets a poisoned parent environment
  redirect execution, which the principle's second clause excludes. There is
  precedent for the alternative: `SANDBOX_EXEC` at
  `src/exec/os_sandbox_seatbelt.rs:32` is an absolute path *precisely* so a scrubbed
  `PATH` cannot hide it. If a nested-shell failure is ever observed, fix it that way
  — with an absolute `cmd.exe` path — not by inheriting `COMSPEC`.
- **`windir`** — same value as `SystemRoot`, and no concrete consumer has been
  named. Excluded pending evidence.
- **`SystemDrive`, `OS`, `HOMEDRIVE`/`HOMEPATH`, `ProgramData`/`ProgramFiles`,
  `NUMBER_OF_PROCESSORS`, `PROCESSOR_ARCHITECTURE`** — batch-script idioms rather
  than what an ordinary command needs to start. Each is non-secret and cheap to add
  later *with a named failing command as evidence*.
- **`HOME`** — normally absent on Windows, so it would be a silent no-op, which is
  the exact failure mode that produced this bug. MSYS/Git-for-Windows set it; if
  that turns out to matter, it should arrive with a repro.

None of these entries carries a credential. The list stays a minimum-to-function
set, not a broad copy of the parent environment.

Windows environment names are case-insensitive at the OS level, and Rust's std
honours that on both the lookup and the child's env map, so the casing above is
cosmetic.

### 2. Expose the default so callers can extend it

`env_allowlist()` **replaces** the default. On Windows that means a caller who
writes `.env_allowlist(["PATH", "MY_VAR"])` silently drops `SystemRoot` and
re-creates this exact bug. The repo's own code does the replacing thing in three
places today (`docs/book/src/concepts/tools.md:388`, `tests/bash.rs:208`,
`tests/host_backend.rs:33`), which is evidence the shape invites it.

The const is `pub` in `exec`, and `lib.rs` re-exports it in one line alongside the
existing `HostBackend`, `ExecOutput`, `Isolation` re-exports:

```rust
pub use exec::DEFAULT_ENV_ALLOWLIST;
```

> **Correction to the first draft.** It claimed an associated const on the backend
> type was "the only way to expose the list", because `mod exec` is private
> (`lib.rs:28`). That is false — a private module is exactly why `lib.rs:39-54`
> re-exports its contents, and one `pub use` does the job. The first draft's plan of
> three separate `pub const DEFAULT_ENV_ALLOWLIST` associated consts (two of them on
> the *same* name `OsSandboxBackend` on different platforms) would have been three
> public items and three doc comments to keep in sync, aliasing one value — and the
> `ignore`d rustdoc example naming `HostBackend::` would have been silently wrong on
> two of the three.

Callers extend rather than replace:

```rust
use paigasus_helikon_tools::DEFAULT_ENV_ALLOWLIST;

.env_allowlist(DEFAULT_ENV_ALLOWLIST.iter().copied().chain(["MY_VAR"]))
```

This is purely additive API. `env_allowlist()` keeps its replace semantics.

`DEFAULT_TIMEOUT` and `DEFAULT_MAX_OUTPUT` have the identical "`pub` inside a
private module, therefore unreachable" problem. Re-exporting them is a tidy
follow-up but is **out of scope here** — this ticket is about the allowlist, and
sweeping them in would widen a defect-fix PR's public surface without a ticket
behind it.

### 3. Read env values losslessly

`spawn_capped` currently does `if let Ok(val) = std::env::var(name)`, which drops
any value that is not valid Unicode — the same silent-no-op class of failure that
produced this bug, and materially more likely on Windows. Switch to
`std::env::var_os`; `cmd.env` already takes `AsRef<OsStr>`, so this is a two-token
change with no downside.

### 4. Documentation

Because **docs.rs builds only `x86_64-unknown-linux-gnu`**, a Windows reader on
docs.rs or crates.io sees the unix arm and nothing else. Saying "the
platform-appropriate default" would therefore reproduce the very
discoverability failure this ticket exists to fix. Every corrected doc comment must
**enumerate both lists literally in prose**, as the const's own doc comment above
does.

Sites to correct, all of which currently hardcode `["PATH","HOME"]`:
`host.rs:41`, `host.rs:97`, `os_sandbox.rs:64`, `os_sandbox_seatbelt.rs:63`.

`env_allowlist()`'s rustdoc additionally gains:

- that it **replaces** rather than extends, and that a Windows list omitting
  `SystemRoot` breaks networked commands;
- that a name absent from the parent environment is dropped **without diagnostic**
  (the crate has no `tracing` dependency, so adding real logging is a separate
  decision, not a freebie here);
- the rollback path for an operator who wants the pre-change minimal environment:
  `.env_allowlist(["PATH"])`.

`docs/book/src/concepts/tools.md:388` passes `.env_allowlist(["PATH", "HOME"])`
explicitly in its `HostBackend` example, which is now actively misleading on
Windows. It becomes a default-using example plus a note on the platform-aware
default and the extend-don't-replace idiom.

The crate `README.md:7` describes `HostBackend` as doing "env scrubbing" without
naming the list, so it needs no edit. Recorded as a conscious call, not a silent
skip.

## Alternatives considered and rejected

**(A) An additive `env_allowlist_extend()` builder method.** Considered and
rejected during brainstorming: it is new public API on three builders that the
ticket did not ask for, and two overlapping methods invite confusion about which one
won. The exported const gets most of the benefit for one public item instead of
three.

**(B) Apply a platform floor (`SystemRoot`, `PATHEXT`) unconditionally inside
`spawn_capped`, regardless of the caller's allowlist.** Rejected. It would make the
fix impossible to opt out of, and — decisively — it breaks the contract that makes
this type testable and reviewable: *the allowlist is the complete description of the
child's environment*. A hidden floor turns that clean, checkable invariant into a
lie, and the unix `env`-set assertion in the test plan below could not be written
against it. Correctness that cannot be asserted is worse than correctness that can.

## Testing

### `tests/exec_timeout_portable.rs`

Loses its `ENV_ALLOWLIST` const and its `.env_allowlist(...)` call, so the file
returns to being purely about timeouts. This satisfies acceptance criterion 2. It is
**not** treated as evidence for criterion 1 — that inference depends on the
unverified `ping` hypothesis.

### `tests/exec_env_defaults.rs` (new, ungated)

Opens with `#![allow(missing_docs)]`, as every integration test in this crate does
(`bash.rs:1`, `host_backend.rs:1`, `exec_timeout_portable.rs:6`) — `clippy
--all-targets -D warnings` covers integration test targets.

Every test here builds the backend **without** calling `env_allowlist()`.

**Windows — the direct probe (primary).** `cmd` echoes a literal `%NAME%` for a
variable that is unset, which makes the assertion deterministic and independent of
any hypothesis about Winsock:

```text
echo [%SystemRoot%][%PATHEXT%][%TEMP%][%USERPROFILE%][%APPDATA%][%LOCALAPPDATA%]
```

Assert `exit_code == Some(0)` and that stdout contains no `%`. This **fails
deterministically without the fix** and covers six of the eight entries directly,
rather than one entry indirectly.

**Windows — the networked smoke (secondary).** `ping -n 1 127.0.0.1`, asserting
`exit_code == Some(0)`. This is acceptance criterion 1's literal wording, so it is
worth having; but it is labelled in-code as a smoke test whose discriminating power
is *unverified*, so no future reader mistakes it for the real guard. Wrapped in an
outer `tokio::time::timeout` — as `exec_timeout_portable.rs` already does — so a
regression surfaces as a fast failure rather than a 30 s stall on a required gate.

**Unix — the scrub assertion.** Runs `env` and asserts on the exported *name set*,
which is a genuine end-to-end guard for criterion 4:

```rust
// `sh` injects a few names of its own: dash exports PWD; bash-as-sh also
// exports SHLVL and _. Assert a subset, so the test is shell-agnostic.
const SH_INJECTED: &[&str] = &["PWD", "SHLVL", "_"];
```

Assert the observed set contains `PATH`, and that every observed name is in
`DEFAULT_ENV_ALLOWLIST ∪ SH_INJECTED`. Widening the unix default to
`["PATH", "HOME", "AWS_SECRET_ACCESS_KEY"]` fails this; the first draft's
`test -n "$PATH" && test -n "$HOME"` would have passed it unchanged. It also cannot
false-fail when the parent has no `HOME` — an absent `HOME` merely yields a smaller
subset — which the first draft's version could, on two required gates.

### `#[cfg(test)] mod tests` in `src/exec/mod.rs` (new)

Named explicitly because the file has no test module today and the first draft's
"alongside it" was ambiguous. The pin belongs on the source of truth rather than on
the re-export.

**Exact-equality** assertions on both platforms, not `contains`:

```rust
#[cfg(unix)]
assert_eq!(DEFAULT_ENV_ALLOWLIST, ["PATH", "HOME"]);
```

with the Windows arm pinning all eight names in order. Exact equality is what stops
a future PR from quietly adding a credential-bearing name to either list; a
`contains("SystemRoot")` assertion would not have noticed.

## Acceptance criteria → coverage

Quoted verbatim from SMA-614, so the mapping is checkable without Linear access.

| Criterion | Covered by |
| --- | --- |
| "A default-configured `HostBackend` can run an ordinary networked command on Windows without a caller-supplied allowlist." | `exec_env_defaults.rs` Windows probe (primary, deterministic) + `ping` smoke (secondary) |
| "The bespoke `ENV_ALLOWLIST` workaround in `tests/exec_timeout_portable.rs` can be deleted, and the test still passes on Windows using the default." | `exec_timeout_portable.rs` diff, verified on `test (windows-latest, stable)` |
| "The doc comment on `HostBackend::builder` (which currently reads `["PATH","HOME"]` env allowlist) is corrected to describe the per-platform default." | `host.rs:97` (and `:41`, `os_sandbox.rs:64`, `os_sandbox_seatbelt.rs:63`), each enumerating both lists literally for docs.rs readers |
| "No widening of the unix default — this is about Windows correctness, not about inheriting more environment everywhere." | Exact-equality const pin in `exec/mod.rs` **and** the unix `env`-set scrub assertion |

## Release mechanics

**Commit and PR type `feat(tools):`, not `fix(tools):`.** `.versionrc` maps `feat`
to Minor and `fix` to Patch. This PR adds a public item (`DEFAULT_ENV_ALLOWLIST`)
and changes runtime behaviour on Windows, so a patch bump would understate it.
`-tools` is at `0.2.16` today and `0.2.17` after release PR #232, so this lands as
**`0.3.0`** — a visible jump, and the honest one.

The CHANGELOG entry must state the env-surface change explicitly — *"on Windows the
default `HostBackend` environment now passes 8 variables to child processes instead
of 1"* — so it is reviewable from the release PR alone rather than being buried
behind a one-line fix description.

No hand-bumped versions: release-plz performs the bump and the facade cascade on
merge.

## Notes for implementation

- Snippets in this document are **not** rustfmt-normalized (`fmt` is a required
  gate). Run `cargo fmt --all` rather than copying formatting verbatim.
- Rebase onto `main` after release PR #232 merges; it touches
  `crates/paigasus-helikon-tools/{Cargo.toml,CHANGELOG.md}`, which this branch does
  not.

## References

- SMA-569 design doc, "Accepted gaps (not fixed here)":
  `docs/superpowers/specs/2026-09-04-exec-timeout-exit-code-design.md`
- forkd env rationale:
  `docs/superpowers/specs/2026-06-21-sma-416-forkd-microvm-spike.md:204`
