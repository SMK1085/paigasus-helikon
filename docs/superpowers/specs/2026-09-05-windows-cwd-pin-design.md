# SMA-615 — Establish, then fix, whether `HostBackend` pins cwd on Windows

**Status:** revised after adversarial challenge
**Date:** 2026-09-05
**Crate:** `paigasus-helikon-tools`
**Linear:** [SMA-615](https://linear.app/smaschek/issue/SMA-615/hostbackend-may-not-actually-pin-cwd-on-windows-sandboxopen)
**Surfaced by:** the whole-branch review on SMA-613; related to SMA-613 and SMA-614.
**Base:** `1d661c8` (`chore: release (#237)`, after SMA-613 and the 0.2.19 release merged).

## Problem

`Sandbox::open` canonicalizes its root and stores the result
(`crates/paigasus-helikon-tools/src/sandbox.rs:35`):

```rust
let canonical = root.canonicalize().map_err(|source| SandboxError::Open {
    path: root.to_path_buf(),
    source,
})?;
```

`Sandbox::root()` returns that path. `HostBackendBuilder::build`
(`src/exec/host.rs:96`) copies it into `ExecConfig::cwd`, and `spawn_capped`
(`src/exec/mod.rs`) hands it to `Command::current_dir`.

On Windows, `std::fs::canonicalize` returns a **verbatim** path —
`\\?\C:\Users\...`. `cmd.exe` inspects its startup working directory and, on a
path beginning `\\`, is documented to print:

> CMD.EXE was started with the above path as the current directory. UNC paths are
> not supported. Defaulting to Windows directory.

and reset its working directory to `%SystemRoot%`.

If that is what happens, `HostBackend` does **not** pin the working directory on
Windows: every command runs from `C:\Windows`, and a relative path in a user's
Bash command resolves somewhere entirely unexpected.

### Every site that claims cwd pinning

Seven, not the three the ticket names. Enumerated here because "which of these
becomes false, and which must be narrowed" is the substance of Goal 3, and a
partial list would leave the highest-impact one unaddressed:

| # | Site | Text | Audience |
| -- | -- | -- | -- |
| 1 | `src/bash.rs:64` | "The working directory is pinned to the sandbox root." | **the model** — this string is in `BashTool`'s tool schema |
| 2 | `src/lib.rs:12` | "is a cwd-pinned shell and **NOT a security boundary**" | crate rustdoc |
| 3 | `src/exec/host.rs:1` | "A cwd-pinned shell with env scrubbing…" | module rustdoc |
| 4 | `src/exec/host.rs:107` | "The default, cwd-pinned execution backend. See the module docs" | struct rustdoc |
| 5 | `README.md:7` | "a cwd-pinned shell with env scrubbing and resource limits" | crates.io landing page |
| 6 | `docs/book/src/concepts/tools.md:185` | "a **cwd-pinned shell, not a security sandbox**" | book |
| 7 | `docs/book/src/concepts/tools.md:379` | "Pins the working directory to the sandbox root" | book, `HostBackend` reference |

Site 1 is the one that matters most: it is what tells the model relative paths are
safe, and is therefore the proximate cause of the misbehaviour the ticket
describes. Per-site disposition is in "Documentation".

### Severity

Correctness, not containment. `Sandbox`'s `cap-std` `Dir` remains the enforcement
boundary for `ReadTool`/`WriteTool`/`EditTool`, and `BashTool` was never an
isolation boundary on any platform (`HostBackend::guarantees()` reports
`Isolation::None` on all three axes). Nothing becomes reachable that was not
already reachable. What breaks is the *predictability* of a relative path.

### The fact is not established

This is the defining constraint on the whole ticket. It could not be reproduced
or refuted from the arm64 macOS dev host, and no existing test covers it:

- `tests/host_backend.rs` — which contains `host_backend_runs_command_in_cwd`,
  the only test that asserts cwd pinning — is file-level `#![cfg(unix)]`.
- `tests/exec_timeout_portable.rs::timeout_reports_no_exit_code` runs
  `ping -n 5 127.0.0.1 >NUL 2>&1`: cwd-independent.
- `tests/exec_timeout_portable.rs::timeout_kills_the_whole_subtree` (SMA-613) was
  *deliberately* made cwd-independent — absolute script and sentinel paths —
  precisely so a red Windows gate there would indict the subtree kill and not
  this unknown.
- `tests/exec_env_defaults.rs` (SMA-614) probes environment variables and would
  tolerate the extra banner lines on stdout without failing.

There is also a live, specific reason the suspicion may be **wrong**. Rust's
`Command::current_dir` does not call `SetCurrentDirectory`; it passes the path as
`CreateProcessW`'s `lpCurrentDirectory`, which accepts verbatim paths. The child's
own `GetCurrentDirectory` may well report the normalized `C:\Users\...` form, in
which case `cmd.exe` never sees a leading `\\` and there is nothing to fix.

**Three worlds, not two.** Collapsing the latter two is what the first draft of
this spec got wrong:

| World | What `cd` prints in the child | Verdict |
| -- | -- | -- |
| **W1** | `C:\Windows` | confirmed — cwd is not pinned |
| **W2** | `C:\Users\…\.tmpXXXX` | refuted — `CreateProcessW` normalized the prefix away |
| **W3** | `\\?\C:\Users\…\.tmpXXXX` | refuted — `cmd.exe` tolerated the verbatim cwd |

W2 and W3 are both "no fix needed", but they are different facts about Windows,
and only W3 justifies the sentence the ticket asks us to leave behind
("the verbatim path is fine as a `cmd.exe` cwd"). Decision 1 instruments all
three.

## Goals

1. **Establish which of W1/W2/W3 holds** on `test (windows-latest, stable)` — the
   required gate that is the only Windows signal in CI — in a single run, with the
   child's raw reported working directory legible in the run's output **in every
   world**.
2. Leave behind a permanent, ungated regression test for cwd pinning, whichever
   way the answer falls.
3. If confirmed: make the seven claims above true on Windows, or narrow the ones
   that stay false.
4. No behaviour change on unix.

## Non-goals

- **No change to containment.** `SandboxGuarantees` is untouched.
- **No `Sandbox::open` failure on an unfixable root.** See Decision 5.
- **No change to `OsSandboxBackend`** (Linux Landlock/seccomp, macOS Seatbelt).
  Both are `cfg`-gated off Windows and the fix is Windows-gated, so the Landlock
  `cwd`/`root` fields (`src/exec/os_sandbox.rs:110`, `:117`) and the Seatbelt
  profile root (`src/exec/os_sandbox_seatbelt.rs:111`) are bit-identical.
- **No hand-bumped version, no hand-edited CHANGELOG.** release-plz owns both
  (CLAUDE.md). Decision 6 pins the commit subject that drives them.

## Constraints discovered

1. **`test (windows-latest, stable)` is the only instrument**, and it is a weak
   linter. `.github/workflows/ci.yml:97` runs `cargo test ${{ matrix.test_args }}`
   with no `RUSTFLAGS`; the only workspace lint is `missing_docs = "warn"`. So
   `unused_variables`/`unused_imports` in a new `#[cfg(windows)]` block are
   **visible in the log but do not fail** the gate, and `clippy` runs on ubuntu
   only. (`src/exec/host.rs:141` currently claims such a warning would be "an
   error under `-D warnings`" on Windows — it would not; `build_command`'s comment
   at `src/exec/mod.rs:445`, which says only "visible", is the accurate one.)
   Mitigation in "Local verification".
2. **`cargo test` has no `--no-fail-fast`,** so it stops after the first failing
   test *target*. A red `tests/exec_cwd.rs` aborts the remaining Windows test
   binaries in the workspace. See Decision 2 for the cost.
3. **`ci.yml:8-10` sets `cancel-in-progress: true` on pull requests.** Any push to
   the branch while round 1 is running cancels the experiment.
4. **No literal `"` survives `spawn_capped` → `cmd /C`.** `Command::arg`'s escaper
   rewrites `"` to `\"`, and `cmd.exe`'s escape character is `^`, not `\`
   (established on SMA-613; `spawns_grandchild`'s doc comment records the
   mechanism). **The design below passes no path through the command string**, so
   this does not bite and no space-in-`TEMP` skip is needed.
5. **The Windows runner's `TEMP` is the 8.3 short form**
   (`C:\Users\RUNNER~1\AppData\Local\Temp`) while `canonicalize` returns the long
   form. Any *contract* assertion must normalize both sides or it fails for a
   spelling reason. (The *oracle* assertion in Decision 1 must NOT normalize —
   that is the whole point of it.)
6. **`dunce` 1.0.5 is already in this workspace's `Cargo.lock`** (`:1911`), as a
   build-dependency of `aws-lc-sys 0.45.0` (`:699`) reached via
   `aws-lc-rs` ← `rustls`/`gcp_auth`. Zero dependencies, no declared
   `rust-version`, licensed `CC0-1.0 OR MIT-0 OR Apache-2.0` — all three already
   in `deny.toml`'s allow list. **But** `paigasus-helikon-tools` with *default*
   features pulls no rustls and therefore no `aws-lc-sys`, so for a downstream
   consumer of the published crate `dunce` is a genuinely new node in the graph
   and a new line in `sbom.yml` output. Its manifest also declares
   `[badges.maintenance] status = "passively-maintained"`. Decision 4 is sized to
   that, not to the zero-cost reading.
7. **`deny.toml` sets `wildcards = "deny"`.** `dunce = "1"` satisfies it (matching
   `cap-std = "4"`); `"*"` would not.
8. **`tests/sandbox.rs:16` is ungated** and asserts
   `sandbox.root() == tmp.path().canonicalize().unwrap()`. It passes on Windows
   today because both sides are verbatim, and would fail after Decision 3.
9. **`docs/superpowers/**` is excluded from `markdown-lint`**
   (`.markdownlint-cli2.jsonc:27`), so this spec and its plan are not linted. The
   book edit **is**.
10. **`.versionrc` marks `fix` as `increment: Patch, section: Fixes, hidden:
    false`** — a `fix` commit produces a visible CHANGELOG line. `test` is
    `increment: None, hidden: true`. This is the whole release-communication
    mechanism; see Decision 6.

## Decisions

### Decision 1 — the probe is the regression test, plus a one-round oracle

A test asserting the **documented contract** — the child's working directory is
the sandbox root — is both the experiment and the permanent guard. Red confirms
W1 with the observed path in the message; green refutes it and stays as the
regression test. No throwaway probe, no deliberately-false assertion, no second PR.

That alone is not enough, because it cannot separate W2 from W3: it compares
*canonicalized* forms, and `cargo test` runs without `--nocapture`, so libtest
swallows a passing test's stdout and the raw `cd` output never reaches the log on
green. Round 2b's source comment would then assert a mechanism nobody observed.

So round 1 also carries a **Windows-only oracle**, `windows_child_reports_a_verbatim_cwd`,
asserting the raw, un-normalized reported path `starts_with(r"\\?\")`. Its
expected answer is unknown by construction — it exists to be read, not to pass:

| World | contract test | oracle test | What the pair tells us |
| -- | -- | -- | -- |
| W1 | **red** (`C:\Windows`) | red | confirmed; go to round 2a |
| W2 | green | **red** (shows `C:\Users\…`) | refuted; `CreateProcessW` normalizes |
| W3 | green | green | refuted; `cmd.exe` tolerates verbatim |

Every world produces at least one red assertion carrying the raw string, so
Goal 1 holds in all three. In round 2 the oracle is rewritten to assert the
observed truth and kept as a pinned characterization test — it is the thing that
goes red if a future Rust or Windows release changes the behaviour underneath us.

**Rejected: assert the *suspicion*** (`cwd != root`). Green only in the buggy
world, so it must be inverted either way, and it encodes a claim we hope to delete.

**Rejected: fix blind** — land the strip and the contract test together. Green
either way, so we would never learn whether the dependency and the public
behaviour change were necessary. The ticket explicitly asks for the fact first.

### Decision 2 — one PR, two rounds, opened as a draft

Round 1 pushes only the tests and opens the PR **as a draft**. When the Windows
gate reports, round 2 pushes the branch CI selected and marks the PR ready.

Draft status is a courtesy, not a guarantee: `.coderabbit.yaml` carries no
`reviews` block at all (it overrides only the Linear integration, and its own
comment records that review behaviour lives at the org level), so whether a draft
is skipped depends on settings not visible from this repo. If CodeRabbit reviews
anyway, the cost is a review of a state we are about to replace.

**The cost of round 1 is more than one cycle** (Constraint 2): a red
`tests/exec_cwd.rs` aborts the remaining Windows test binaries, so an unrelated
Windows regression introduced by the same push stays invisible until round 2.
Accepted rather than worked around — renaming the file to sort last would be
obscure, and round 2 re-runs the full suite anyway.

**Do not push again while round 1 is in flight** (Constraint 3):
`cancel-in-progress` would cancel the experiment.

**Reading the gate** — use the Checks API, not the legacy commit-status API
(CLAUDE.md), and read the *failure output*, never just the conclusion; a red
Windows leg from an unrelated flake must not be misread as confirmation:

```bash
gh api repos/SMK1085/paigasus-helikon/commits/<sha>/check-runs \
  --jq '.check_runs[] | select(.name | startswith("test (windows")) | {name, conclusion, id}'
gh run view <run-id> --log-failed | grep -A20 'exec_cwd'
```

The verdict is which of the two named tests failed and what their messages
printed — anything else is a flake.

### Decision 3 — if confirmed, strip in `Sandbox::open`

`root()` returns the simplified form on Windows; every consumer is fixed at one
choke point. Its own rustdoc already reads "The canonical sandbox root on the host
filesystem (diagnostics / cwd)" — it is *already* the declared source of truth for
the cwd, so the defect is in what it returns.

Three alternatives were considered:

- **Strip only at `ExecConfig::cwd`** (`HostBackendBuilder::build`). Narrowest
  blast radius, no public change — but the knowledge lives inside one backend.
- **`pub(crate) fn spawn_root(&self) -> Cow<'_, Path>`.** Genuinely attractive:
  the same single choke point for all four internal `root()` consumers, zero
  public behaviour change, `tests/sandbox.rs` untouched, no downstream surprise.
  **Rejected because `Sandbox` and `ExecutionBackend` are both public.** A
  third-party backend — the exact extension point that trait exists for — reaches
  for `sandbox.root()` to build its cwd and walks straight into this bug, and a
  crate-private helper cannot save it. Whatever `root()` hands out must be usable
  as a cwd.
- **Two public accessors** (`root()` verbatim, `spawn_root()` simplified).
  Permanent public API whose only purpose is to describe a Windows quirk, with
  every caller obliged to know which to reach for. YAGNI.

Nothing needs the verbatim form: `cap-std`'s `Dir` holds the real capability.
`root()`'s complete consumer set, grepped across the workspace, is four internal
sites — `src/exec/host.rs:96`, `src/exec/os_sandbox.rs:110` and `:117`,
`src/exec/os_sandbox_seatbelt.rs:111` — plus `tests/sandbox.rs:16`. The three
`os_sandbox*` sites are Linux/macOS-only and the fix is Windows-gated, so they are
untouched.

### Decision 4 — `dunce`, gated to Windows

`dunce::canonicalize` is `fs::canonicalize` followed by `simplified`, which strips
`\\?\` **only when the result has a valid traditional spelling** — the envelope
Decision 5 enumerates. That is a solved, battle-tested problem sitting in our
lockfile; re-deriving it would mean ~40 lines plus tests for six separate edge
classes.

**Gated to `[target.'cfg(windows)'.dependencies]`, not unconditional.** The first
draft made it unconditional on the grounds that the supply-chain cost was zero.
Constraint 6 shows that is only true of *this workspace's* `--all-features`
lockfile: a default-features downstream consumer would gain a new, passively-maintained
graph node for a function that is the identity on their platform. Gating costs
three lines in `Sandbox::open` and makes "unix is untouched" structural rather
than a promise:

```rust
#[cfg(windows)]
let canonical = dunce::canonicalize(root);
#[cfg(not(windows))]
let canonical = root.canonicalize();
let canonical = canonical.map_err(|source| SandboxError::Open {
    path: root.to_path_buf(),
    source,
})?;
```

**Rejected: `std::path::absolute()`** (stable 1.79, zero deps, non-verbatim). Not
equivalent — it neither resolves symlinks nor expands 8.3 short names, so `root()`
would become `C:\Users\RUNNER~1\...`, silently weakening the canonicalization that
`Sandbox::open` exists to perform and that `tests/sandbox.rs` asserts.

**Rejected: hand-rolled prefix strip.** See above.

### Decision 5 — an unfixable root degrades with a warning, never an error

Read from `dunce-1.0.5/src/lib.rs:145-182`, `is_safe_to_strip_unc` returns `false`
— and the verbatim prefix therefore survives — for **six** classes, not the two
the first draft claimed:

1. Any prefix that is not `Prefix::VerbatimDisk` — `\\?\UNC\` network shares,
   `\\.\`, `\\?\GLOBALROOT\`.
2. Any `.` or `..` component (unreachable for us: `Sandbox::open` canonicalizes
   first).
3. Any component that is a reserved DOS name — `CON`, `NUL`, `PRN`, `AUX`,
   `COM1`–`COM9`, `LPT1`–`LPT9`, including `con.txt` and `PrN...`.
4. Any component ending in `.` or a space, containing `<>:"/\|?*` or a control
   byte, or longer than 255 characters.
5. Total length over 260, measured on both `len()` and `windows_char_len()`
   including the 4-byte prefix.
6. A non-Unicode path — `simplified` bails through `path.to_str()`; its own docs
   note "paths with unpaired surrogates aren't converted".

Correcting the first draft's wording: for class 5 a traditional spelling *does*
exist; what does not exist is one that legacy APIs handle safely. Class 1 is the
genuinely irreducible case — the traditional spelling `\\server\share\…` is
*itself* what `cmd.exe` rejects, so no spelling fixes it.

For all six, cwd pinning still does not hold under `cmd.exe`. `Sandbox::open` must
**not** fail: that would break working long-path and network-share setups to
satisfy a documentation claim, on a path where the FS tools continue to work
correctly (`cap-std` is unaffected) and only `BashTool`'s relative-path resolution
degrades.

**The warning goes in `HostBackendBuilder::build`, not `Sandbox::open`.** A
`Sandbox` is shared by `ReadTool`/`WriteTool`/`EditTool`, none of which care about
cwd; warning at `Sandbox::open` would fire for sandboxes that never run a command
— once per request in a server — training operators to ignore the very target
SMA-613 uses for real degradation. Warning where the object that *claims* cwd
pinning is constructed fires once per backend and makes
`target: "paigasus::tools::exec"` correct. Detection uses dunce's own documented
idiom rather than a lossy string compare:

```rust
#[cfg(windows)]
if cwd.as_os_str().as_encoded_bytes().starts_with(b"\\\\") { tracing::warn!(...) }
```

Note this does not re-open Decision 3 — the strip still happens in
`Sandbox::open`; only the diagnostic moves.

**Accepted untested.** Exercising the degrade path needs a >260-character root,
which requires long-path support that is not guaranteed on the runner, or a
reserved-name directory. dunce's own tests cover `is_safe_to_strip_unc`; ours
would only cover the three-line `starts_with` glue. Recorded as a conscious gap.

### Decision 6 — pin the commit subjects; `fix` is the release channel

`Sandbox::root()` is `pub` on a published 0.2.x crate and round 2a changes what it
returns on Windows. release-plz generates the CHANGELOG from the commit subject
(Constraint 10), so the subject *is* the downstream notification:

- **Round 2a (confirmed):**
  `fix(tools): SMA-615 strip the verbatim prefix from the sandbox root so windows honours the cwd`
  → `increment: Patch`, section **Fixes**, not hidden. 0.2.19 → 0.2.20 with a
  visible line naming the behaviour change.
- **Round 2b (refuted):**
  `test(tools): SMA-615 pin the host backend working directory on every platform`
  → `increment: None`, hidden. Correct: nothing behavioural changed.

Both satisfy `pr-title.yml`'s two independent rules — a valid `type(scope):` from
the allowlist with `tools` a valid scope, and a subject whose first character
after `SMA-615 ` is lowercase (`subjectPattern: ^([A-Z]{2,4}-\d+ )?[^A-Z].+$`).
The PR title takes the round-2 form, since it is what becomes the squashed `main`
commit.

## Design

### Round 1 — `crates/paigasus-helikon-tools/tests/exec_cwd.rs` (new, ungated)

A new file rather than an addition to `exec_timeout_portable.rs`: that file is
about timeouts, and `exec_env_defaults.rs` already set the precedent that each
portable exec concern gets its own ungated file.

Shared helper: run a command through a real `HostBackend` over a `tempfile`
sandbox and return `(ExecOutput, sandbox_root)`. Every assertion message in this
file embeds **both** `out.stdout` and `out.stderr` verbatim — the banner's stream
is not known in advance, and the message is the experiment's only readout.
Each `backend.run(...)` is wrapped in a `tokio::time::timeout` (10s; the commands
are instant and the backend timeout is 10s), matching
`exec_timeout_portable.rs:48` and `exec_env_defaults.rs:166`, so a regression
fails fast instead of stalling a required gate.

**Test A — `host_backend_pins_cwd_to_the_sandbox_root`** (ungated, the contract).

Runs `pwd` (unix) / `cd` (Windows). Then:

- Assert `exit_code == Some(0)` first, so a shell that failed to start is not read
  as a cwd verdict.
- Extract the raw path: `stdout.lines().rev().find(|l| !l.trim().is_empty())`,
  with `unwrap_or_else(|| panic!(…))` carrying both streams — *last* non-empty
  line, because a UNC banner precedes `cd`'s output.
- On Windows, assert neither stream contains `"UNC paths are not supported"`.
  The banner and the `%SystemRoot%` reset are the same `cmd.exe` code path, so its
  presence is confirmation on its own, independent of what `cd` printed.
- Canonicalize both the observed path and `sandbox.root()` and compare, with
  `map_err`/`unwrap_or_else` panics carrying both streams — never a bare
  `.unwrap()`, which would surface an `io::Error` and discard the readout on
  exactly the paths where it matters most.

Canonicalizing both sides normalizes 8.3 short names (Constraint 5), case, and
verbatim-ness. It does **not** paper over the question: canonicalization
normalizes spelling, not identity, so a cwd of `C:\Windows` still yields
inequality and a red gate.

**Test B — `host_backend_resolves_a_relative_path_in_the_sandbox`** (ungated, the
behaviour). Writes `marker.txt` containing short, newline-free, `%`-free ASCII
into the sandbox, runs `cat marker.txt` (unix) / `type marker.txt` (Windows),
asserts exit 0 and `stdout.contains(...)`. Content is pinned to that shape so the
test cannot fail for an echo-expansion reason.

Not redundant with Test A: it fails with a different message, so a red gate
distinguishes "the cwd is elsewhere" from "the two spellings differ". It is the
ungated counterpart of `host_backend_runs_command_in_cwd`
(`tests/host_backend.rs:7`), which stays where it is — that file's other three
tests are unix-only for `rlimit` reasons, and deleting one test from it to avoid a
two-line overlap would cost more clarity than it saves.

**Test C — `windows_child_reports_a_verbatim_cwd`** (`#[cfg(windows)]`, the
oracle). Asserts the raw extracted path — *not* canonicalized —
`starts_with(r"\\?\")`, with a message naming the observed string. Its doc comment
must state plainly that it is a round-1 oracle whose expected answer is unknown,
and that round 2 rewrites it to the observed truth. Per Decision 1 this is what
separates W2 from W3.

**Drive-by correction.** `tests/exec_timeout_portable.rs`'s module doc claims to
be the only ungated real-process exec file. Already false when
`exec_env_defaults.rs` landed (SMA-614); this makes a third. The corrected
sentence must distinguish *ungated* from *unix-gated* precisely — `host_backend.rs`
and `exec_env_non_unicode.rs:8` are the `#![cfg(unix)]` ones; `exec_env_defaults.rs`
and `exec_cwd.rs` are ungated.

### Round 2a — if confirmed (W1)

| File | Change |
| -- | -- |
| `Cargo.toml` (root) | `dunce = "1"` in `[workspace.dependencies]` |
| `crates/paigasus-helikon-tools/Cargo.toml` | `dunce = { workspace = true }` under `[target.'cfg(windows)'.dependencies]`, beside `windows-sys` |
| `Cargo.lock` | regenerated (committed — CLAUDE.md) |
| `src/sandbox.rs` | the Decision 4 `cfg` pair; `root()` rustdoc reworded |
| `src/exec/host.rs` | Decision 5 warning in `build`; module doc caveat; fix the wrong `-D warnings` comment at `:141` (Constraint 1) |
| `tests/exec_cwd.rs` | Test C rewritten to the observed truth |
| `tests/sandbox.rs:16` | see below |
| `docs/book/src/concepts/tools.md` | site 7 caveat |

`tests/sandbox.rs:16` becomes a comparison of *canonicalized* forms
(`sandbox.root().canonicalize().unwrap()` vs `tmp.path().canonicalize().unwrap()`),
which is true on both platforms and still asserts what the test means — that
`root()` names the same directory. A `#[cfg(windows)]` companion asserts the
prefix was actually stripped. This deliberately avoids making `dunce` reachable
from the test, which a Windows-gated dependency would complicate.

### Round 2b — if refuted (W2 or W3)

Tests A and B stay as permanent regression guards; Test C is rewritten to assert
whichever of W2/W3 round 1 observed, and kept. Add the comment the ticket asks for
at `src/sandbox.rs:35` — recording **only what was observed**, scoped to where it
was observed, and pointing at `tests/exec_cwd.rs` as the standing evidence. It
must not assert a mechanism the experiment did not distinguish, and it must not
generalize a single runner image into a claim about every Windows host (see
"Risks"). No `dunce`, no manifest change, no public behaviour change.

## Testing

| Test | File | Runs on |
| -- | -- | -- |
| A `host_backend_pins_cwd_to_the_sandbox_root` | `tests/exec_cwd.rs` (new) | all six `test` matrix legs |
| B `host_backend_resolves_a_relative_path_in_the_sandbox` | `tests/exec_cwd.rs` (new) | all six |
| C `windows_child_reports_a_verbatim_cwd` | `tests/exec_cwd.rs` (new) | Windows legs only |
| `open_succeeds_on_existing_dir` (amended, 2a only) | `tests/sandbox.rs` | all six |

Only the Windows legs carry new information; the unix legs guard Goal 4 — on unix
the `cfg(not(windows))` arm is the untouched `fs::canonicalize`, so A and B must
be green locally on macOS before round 1 is pushed.

### Local verification (macOS, before each push)

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test -p paigasus-helikon-tools
cargo build --workspace --locked            # round 2a: catches a stale Cargo.lock
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
npx markdownlint-cli2                       # round 2a only (book page)
mdbook build docs/book                      # round 2a only
```

Round 2a additionally requires a **cross-target lint of the Windows-only code**,
because no CI gate lints it (Constraint 1):

```bash
rustup target add x86_64-pc-windows-msvc
cargo clippy -p paigasus-helikon-tools --target x86_64-pc-windows-msvc \
  --all-targets -- -D warnings              # check-only; no linker needed
```

If that cannot be made to work from this host, the gap is accepted explicitly and
recorded on the PR rather than silently.

Local runs cannot answer the question. They only prove the change is green on the
platform that was never in doubt.

## Documentation

Per-site disposition of the seven claims, per Goal 3. **Round 2a only** — in
round 2b every claim was already true and nothing below changes.

| # | Site | Action |
| -- | -- | -- |
| 1 | `src/bash.rs:64` (model-facing) | **No change, deliberately.** After the fix the sentence is true for every sandbox but the Decision 5 residuals, and a caveat there is prompt bloat the model cannot act on — it does not know the root and has no alternative behaviour available. Making it conditional would mean threading a `cwd_pinned` flag from backend through `guarantees()` into the description: a public API change out of proportion to the case. Worth a follow-up ticket, not this PR. |
| 2 | `src/lib.rs:12` | No change — true after the fix. |
| 3 | `src/exec/host.rs:1` | **Add the residual caveat.** The natural home for it. |
| 4 | `src/exec/host.rs:107` | No change — already defers to the module docs, which now carry the caveat. |
| 5 | `README.md:7` | **No change.** The one-line roster claim stays accurate; the platform caveat belongs in the book and rustdoc, not the crates.io landing page. A conscious skip per CLAUDE.md's README rule, not a silent one. |
| 6 | `tools.md:185` | No change — that sentence exists to make the *security* disclaimer, and it still does. |
| 7 | `tools.md:379` | **Add the residual caveat.** The primary reference site. Must stay clean under `npx markdownlint-cli2` and `mdbook build docs/book` (`warning-policy = "error"`). |

Plus `Sandbox::root()`'s rustdoc, which needs **rewording, not extension**: it
currently says "The **canonical** sandbox root", and after Decision 3 the value is
canonical-then-simplified, which on Windows is not the canonical path.

The caveat wording, wherever it appears, should describe the envelope rather than
enumerate all six classes: *the prefix is kept whenever no safe traditional
spelling exists — a network share, a path over 260 characters, a reserved DOS name
or otherwise legacy-invalid component, or a non-Unicode path — and cwd pinning
does not hold for such a root.*

No CHANGELOG edit; Decision 6 covers the release note.

## Risks

| Risk | Mitigation |
| -- | -- |
| The Windows gate is red from an unrelated flake and is misread as confirmation | Decision 2: read the failure *output*. The three tests fail with distinct, specific messages; anything else is a flake. |
| A red gate whose message does not identify the observed cwd | Every assertion and every extraction/canonicalization failure path carries both raw streams. No bare `.unwrap()`. |
| The banner lands on stderr and is never seen | Both streams are captured and searched for the banner text on Windows, and both appear in every message. |
| A green gate is read as "verbatim is fine" when in fact `CreateProcessW` normalized | Test C separates W2 from W3 (Decision 1). |
| **Generalizing one runner image to every Windows host** | Real and unmitigable from here: `HKCU\…\Command Processor\DisableUNCCheck` and `cmd.exe` differences across Server 2019/2022/2025 can change the answer, and `windows-latest` is one image. Round 2b's comment must be scoped to what was observed ("on `windows-latest`, as of <date>, …"), not written as a universal claim about `cmd.exe`. |
| Round 1's red gate hides an unrelated Windows regression | Accepted and stated (Constraint 2, Decision 2); round 2 re-runs the full suite. |
| A push during round 1 cancels the experiment | Constraint 3; Decision 2 says do not push until the leg reports. |
| New `#[cfg(windows)]` code is linted by no CI gate | Constraint 1; cross-target clippy in "Local verification", or an explicit recorded gap. |
| `dunce` changes the unix build | It is not in the unix build at all (Decision 4 gates it to `cfg(windows)`), and the unix `test` legs are the guard. |
| A residual-class root remains unpinned after the fix | Accepted, warned at backend construction, and documented (Decision 5). Erroring instead would break working setups. |
