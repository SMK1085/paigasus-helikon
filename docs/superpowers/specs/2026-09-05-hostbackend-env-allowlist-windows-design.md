# HostBackend's default `env_allowlist` on Windows — design

**Ticket:** [SMA-614](https://linear.app/smaschek/issue/SMA-614/hostbackends-default-env-allowlist-is-unix-shaped-and-breaks-networked)
**Date:** 2026-09-05
**Status:** approved

## Problem

`HostBackend::builder` defaults `env_allowlist` to `["PATH", "HOME"]`
(`crates/paigasus-helikon-tools/src/exec/host.rs:102`). `spawn_capped` calls
`.env_clear()` (`src/exec/mod.rs:217`) and then re-adds only allowlisted names, so
the allowlist is the *entire* environment the child sees.

`HOME` does not exist on Windows, so `std::env::var` returns `Err` and the entry is
silently a no-op. A default-configured `HostBackend` on Windows therefore hands the
child exactly one variable: `PATH`. That breaks ordinary commands in ways that look
like the *command* is wrong rather than the backend's default:

- **`SystemRoot`** — Winsock resolves its provider DLLs through `%SystemRoot%`.
  Without it, socket and ICMP programs fail to initialize.
- **`PATHEXT`** — `cmd.exe`'s executable-extension resolution degrades to its
  built-in fallback rather than the machine's configured list.
- **`TEMP` / `TMP`** — anything writing a temp file has no valid target. Unlike
  unix, Windows has no hardcoded `/tmp`; `GetTempPath` degrades to the Windows
  directory, which is typically not writable.

The documented default — "a sensible minimal environment" — is only sensible on
unix.

### How it surfaced

SMA-569 added `tests/exec_timeout_portable.rs`, the crate's first real-process exec
test that is not `cfg`-gated to unix. It needs a command that blocks past a 200 ms
timeout; on Windows that is `ping`. Under the default allowlist `ping.exe` fails
Winsock init and exits in milliseconds — *before* the timeout fires — making
`timed_out` false and the test fail for a reason unrelated to what it asserts. The
test carries its own Windows allowlist as a workaround, which keeps the test honest
but leaves the product default wrong and obliges every future Windows caller to
rediscover the same list.

## Scope

The other two backends were checked, as the ticket asked. **Neither has the bug.**
`os_sandbox.rs` (Landlock) is gated on `target_os = "linux"` and
`os_sandbox_seatbelt.rs` on `target_os = "macos"` (`src/exec/mod.rs:15-31`), so
their identical `["PATH", "HOME"]` defaults are correctly unix-shaped and cannot be
reached on Windows. What they *do* have is a third and fourth copy of the same
literal, which this design de-duplicates.

Out of scope: any change to what the unix default passes through.

## Design

### 1. One shared platform-aware const

In `src/exec/mod.rs`, beside the existing `DEFAULT_TIMEOUT` / `DEFAULT_MAX_OUTPUT`:

```rust
#[cfg(unix)]
pub(crate) const DEFAULT_ENV_ALLOWLIST: &[&str] = &["PATH", "HOME"];

#[cfg(windows)]
pub(crate) const DEFAULT_ENV_ALLOWLIST: &[&str] = &[
    "PATH", "SystemRoot", "windir", "PATHEXT",
    "TEMP", "TMP", "USERPROFILE", "COMSPEC",
];
```

All three builders drop their literal and collect from the const:

```rust
env_allowlist: DEFAULT_ENV_ALLOWLIST.iter().map(|s| (*s).to_owned()).collect(),
```

**No `cfg(not(any(unix, windows)))` arm.** `build_command` (`src/exec/mod.rs:293`)
already has only `#[cfg(unix)]` and `#[cfg(windows)]` bodies, so the crate already
fails to compile on any other target. A third arm here would advertise portability
the crate does not have.

The `cfg` is inert in the two OS-sandbox builders — they are unix-gated, so they
always see `["PATH", "HOME"]` and their behaviour is byte-for-byte unchanged.

#### Why each Windows entry

| Name | Why it is in the default |
| --- | --- |
| `PATH` | Resolves `cmd` itself and every program the command names. |
| `SystemRoot` | Winsock provider DLL resolution; broadly, system DLL loading. |
| `windir` | Legacy alias for the same directory; some tooling reads only this one. |
| `PATHEXT` | `cmd.exe` extension resolution matches the machine's configuration. |
| `TEMP`, `TMP` | Windows has no hardcoded writable temp path the way unix has `/tmp`. |
| `USERPROFILE` | The `HOME` analogue. Without it the Windows default would be *narrower* than unix, so a command reading the user's home would work on unix and not on Windows. |
| `COMSPEC` | `cmd.exe` reads it to locate itself when spawning a nested shell (pipes, `start`, `for /f`). It falls back to `%SystemRoot%\system32\cmd.exe`, so this is belt-and-braces. |

None of these carry credentials. The point of `env_clear()` plus an allowlist is to
scrub inherited secrets, and this list stays a minimum-to-function set rather than a
broad copy of the parent environment.

Windows environment names are case-insensitive at the OS level, and Rust's std
honours that on both the lookup (`std::env::var`) and the child's env map, so the
casing above is cosmetic.

### 2. Expose the default so callers can extend it

`env_allowlist()` **replaces** the default. On Windows that means a caller who
writes `.env_allowlist(["PATH", "MY_VAR"])` silently drops `SystemRoot` and
re-creates this exact bug. `mod exec` is private in `lib.rs`, so the existing
`pub const DEFAULT_TIMEOUT` is unreachable from outside the crate — an associated
const on the backend type is the only way to expose the list.

Each of the three backends gains:

```rust
impl HostBackend {
    /// The platform-appropriate default env allowlist. Extend it rather than
    /// replacing it:
    ///
    /// ```ignore
    /// .env_allowlist(
    ///     HostBackend::DEFAULT_ENV_ALLOWLIST.iter().copied().chain(["MY_VAR"]),
    /// )
    /// ```
    pub const DEFAULT_ENV_ALLOWLIST: &'static [&'static str] =
        super::DEFAULT_ENV_ALLOWLIST;
}
```

The `super::` qualifier is load-bearing for readability rather than for
resolution — a bare path inside an `impl` block resolves to the module-scope item,
since associated consts need `Self::` — but writing it out keeps the two
same-named items visibly distinct.

This is purely additive API. Behaviour is unchanged; `env_allowlist()` keeps its
replace semantics.

### 3. Documentation

`env_allowlist()`'s rustdoc gains an explicit warning that it replaces rather than
extends, and that a Windows list omitting `SystemRoot` breaks networked commands.

Four doc comments currently hardcode `["PATH","HOME"]` in prose and are corrected to
describe the per-platform default: `host.rs:41`, `host.rs:97`, `os_sandbox.rs:64`,
`os_sandbox_seatbelt.rs:63`.

`docs/book/src/concepts/tools.md:388` passes `.env_allowlist(["PATH", "HOME"])`
explicitly in its `HostBackend` example, which is now actively misleading on
Windows. It becomes a default-using example plus a note on the platform-aware
default and the extend-don't-replace idiom.

The crate `README.md` describes `HostBackend` as doing "env scrubbing" without
naming the list, so it needs no edit. Recorded as a conscious call, not a silent
skip.

## Testing

`tests/exec_timeout_portable.rs` loses its `ENV_ALLOWLIST` const and its
`.env_allowlist(...)` call, so the file returns to being purely about timeouts. This
alone satisfies acceptance criterion 2 and *implicitly* proves criterion 1 — if
Winsock init failed, `ping` would exit in milliseconds and `timed_out` would be
false — but a reader would see a timeout test, not an env-default test.

A new ungated `tests/exec_env_defaults.rs` therefore carries the assertion
explicitly, keeping each test file single-purpose:

```rust
/// Windows: the acceptance criterion's "ordinary networked command". A loopback
/// `ping` is exactly what fails when Winsock cannot resolve its provider DLLs
/// through %SystemRoot%.
#[cfg(windows)]
const DEFAULT_ENV_OK: &str = "ping -n 1 127.0.0.1";

/// Unix: SMA-614 does not change the unix default, so this leg is a
/// non-regression guard — it exits 1 if PATH or HOME ever stops being passed.
#[cfg(unix)]
const DEFAULT_ENV_OK: &str = r#"test -n "$PATH" && test -n "$HOME""#;
```

built into a test that constructs the backend **without** calling
`env_allowlist()` and asserts `timed_out == false` and `exit_code == Some(0)`.

The unix leg is deliberately **not** a `ping`. Unprivileged ICMP availability varies
by container and runner, and `test (ubuntu-latest, stable)` and
`test (macos-latest, stable)` are required status checks — a flake there would cost
more than the weaker unix assertion buys, and unix behaviour is unchanged by this
ticket anyway.

Alongside it, cfg'd unit assertions pin the const's contents: on Windows that it
contains `SystemRoot` and `PATHEXT`; on unix that it is *exactly* `["PATH", "HOME"]`,
which is the mechanical guard for acceptance criterion 4.

## Acceptance criteria → coverage

| Criterion | Covered by |
| --- | --- |
| Default `HostBackend` runs an ordinary networked command on Windows | `tests/exec_env_defaults.rs`, Windows leg (`ping -n 1 127.0.0.1`, exit 0) |
| The `ENV_ALLOWLIST` workaround can be deleted and the test still passes | `tests/exec_timeout_portable.rs` diff, verified on `test (windows-latest, stable)` |
| `HostBackend::builder`'s doc comment describes the per-platform default | `host.rs:97` (and `:41`, plus the two OS-sandbox builders) |
| No widening of the unix default | Unix unit assertion that the const equals `["PATH", "HOME"]` exactly |

## Release mechanics

No hand-bumped versions: this is an ordinary feature-branch PR, so release-plz
performs the bump and CHANGELOG cascade on merge. Commit and PR type `fix(tools):` —
a defect fix, with the new associated const riding along as additive API.

## References

- SMA-569 design doc, "Accepted gaps (not fixed here)":
  `docs/superpowers/specs/2026-09-04-exec-timeout-exit-code-design.md`
