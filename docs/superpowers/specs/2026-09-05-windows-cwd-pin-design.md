# SMA-615 — Establish, then fix, whether `HostBackend` pins cwd on Windows

**Status:** draft (pre-challenge)
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
Windows: every command runs from `C:\Windows`. A relative path in a user's Bash
command resolves somewhere entirely unexpected, and three documented claims are
false on that platform:

- `src/exec/host.rs:1` — "A cwd-pinned shell with env scrubbing…"
- `HostBackend::builder` rustdoc — "(cwd = `sandbox.root()`)"
- `docs/book/src/concepts/tools.md:379` — "Pins the working directory to the
  sandbox root"

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
which case `cmd.exe` never sees a leading `\\` and there is nothing to fix. The
ticket's suspicion and this refutation are equally plausible from here; only a
Windows run separates them.

## Goals

1. **Establish the fact** on `test (windows-latest, stable)` — the required gate
   that is the only Windows signal in CI — in a single run, with the observed
   working directory legible in the run's output.
2. Leave behind a permanent, ungated regression test for cwd pinning, whichever
   way the answer falls.
3. If confirmed: make the three documented claims true on Windows, or narrow them
   to exactly what holds.
4. No behaviour change on unix.

## Non-goals

- **No change to containment.** `SandboxGuarantees` is untouched.
- **No `Sandbox::open` failure on an unfixable root.** See Decision 5.
- **No change to `OsSandboxBackend`** (Linux Landlock/seccomp, macOS Seatbelt).
  Both are `cfg`-gated off Windows, and the chosen fix is a no-op on unix, so the
  Seatbelt profile root at `src/exec/os_sandbox_seatbelt.rs:111` is bit-identical.
- **No version bump, no CHANGELOG edit.** release-plz owns both (CLAUDE.md);
  this PR bumps nothing, so the release PR that follows the merge is what
  publishes.

## Constraints discovered

1. **`test (windows-latest, stable)` is the only instrument.** It runs
   `cargo test --workspace --all-features`, so an ungated integration test in
   `paigasus-helikon-tools` is executed there. `clippy` runs on ubuntu only, so a
   Windows-only `unused_variables`/`unused_imports` would be caught by this gate
   and not by `clippy` — the same trap `src/exec/host.rs:139` and
   `build_command`'s Windows arm already carry comments about.
2. **No literal `"` survives `spawn_capped` → `cmd /C`.** `Command::arg`'s
   escaper rewrites `"` to `\"`, and `cmd.exe`'s escape character is `^`, not `\`,
   so the backslashes arrive as literal text (established on SMA-613;
   `spawns_grandchild`'s doc comment records the full mechanism). Any design that
   reaches for a quoted path *inside the command string* hits this wall.
   **The design below never does** — it passes no paths through the command
   string at all.
3. **The Windows runner's `TEMP` is the 8.3 short form**
   (`C:\Users\RUNNER~1\AppData\Local\Temp`), while `canonicalize` returns the long
   form. Any assertion comparing an observed path against `sandbox.root()` must
   normalize both sides or it fails for a spelling reason that has nothing to do
   with the question.
4. **`dunce` 1.0.5 is already in `Cargo.lock`** (build-dependency of
   `aws-lc-sys`), has zero dependencies, declares no `rust-version`, and is
   licensed `CC0-1.0 OR MIT-0 OR Apache-2.0` — all three already in `deny.toml`'s
   `[licenses] allow` list. Promoting it to a direct dependency adds no crate to
   the graph and no new license to clear.
5. **`deny.toml` sets `wildcards = "deny"`.** A `dunce = "1"` requirement in
   `[workspace.dependencies]` satisfies it (matching the existing `cap-std = "4"`
   style); a `"*"` would not.
6. **`tests/sandbox.rs:16` is ungated** and asserts
   `sandbox.root() == tmp.path().canonicalize().unwrap()`. It currently passes on
   Windows because both sides are verbatim. Any fix applied at `Sandbox::open`
   must update it; a fix applied at `ExecConfig::cwd` would not. This is the one
   real cost of Decision 3.
7. **`docs/superpowers/**` is excluded from the `markdown-lint` gate**
   (`.markdownlint-cli2.jsonc`), so this spec and its plan are not linted. The
   book page edit in "Documentation" **is** linted.

## Decisions

### Decision 1 — the probe *is* the regression test

The ticket proposes establishing the fact first and adding a test afterwards.
Those are the same artifact. A test asserting the **documented contract** — the
child's working directory is the sandbox root — answers the question on its first
CI run:

- **Red** ⇒ suspicion confirmed, and the assertion message carries the observed
  path (`C:\Windows`) and the raw stdout.
- **Green** ⇒ suspicion refuted, with a permanent guard left in place.

No throwaway probe, no deliberately-false assertion, no second PR. The cost is
one CI cycle on a PR whose Windows gate may be red by design.

**Rejected: assert the *suspicion*** (`cwd != root` on Windows). It is green only
in the world where the bug exists, so it must be inverted in round 2 either way,
and it encodes a claim we hope to delete.

**Rejected: fix blind.** Land the strip and the contract test together. Green
either way, because stripping a prefix `cmd.exe` would have accepted anyway is
harmless — and we would never learn whether the dependency and the public
behaviour change were necessary. The ticket explicitly asks for the fact first.

### Decision 2 — one PR, two rounds, opened as a draft

Round 1 pushes only the test and opens the PR **as a draft**. When the Windows
gate reports, round 2 pushes the branch CI selected and marks the PR ready, at
which point CodeRabbit reviews the finished change.

Draft status is a courtesy, not a guarantee: `.coderabbit.yaml` carries no
`reviews` block at all (it overrides only the Linear integration; its own comment
records that review behaviour lives at the org level), so whether a draft is
skipped depends on settings not visible from this repo. CodeRabbit's default is
to skip drafts, and if it reviews anyway the cost is a review of a state we are
about to replace — see "Risks".

This puts a CI checkpoint in the middle of the pipeline's implementation stage.
That is deliberate and is the only ordering that gets the answer without a
throwaway PR.

**Reading the gate:** use the Checks API, not the legacy commit-status API
(CLAUDE.md), and read the *failure output*, not just the conclusion — a red
Windows gate from an unrelated flake must not be mistaken for confirmation:

```bash
gh api repos/SMK1085/paigasus-helikon/commits/<sha>/check-runs \
  --jq '.check_runs[] | select(.name | startswith("test (windows")) | {name, status, conclusion}'
```

### Decision 3 — if confirmed, strip in `Sandbox::open`

`root()` returns the simplified form on Windows; every consumer is fixed at one
choke point.

The alternative sites were considered and rejected:

- **Strip only at `ExecConfig::cwd`** (`HostBackendBuilder::build`). Narrowest
  blast radius, no public behaviour change, `tests/sandbox.rs` untouched — but
  the knowledge lives inside one backend, so the next backend that spawns a
  process on Windows rediscovers the bug. `Sandbox::root()`'s own rustdoc already
  reads "The canonical sandbox root on the host filesystem (diagnostics / cwd)":
  it is *already* the declared source of truth for the cwd, so the defect is in
  what it returns.
- **Two accessors** (`root()` verbatim, `spawn_root()` simplified). Most explicit,
  no existing behaviour changes — but it is permanent public API whose only
  purpose is to describe a Windows quirk, and every caller must know which to
  reach for. YAGNI.

Nothing needs the verbatim form: `cap-std`'s `Dir` holds the real capability, and
`root()`'s three consumers are the two backends' cwd (`src/exec/host.rs:96`,
`src/exec/os_sandbox.rs:110`), the Seatbelt profile root
(`src/exec/os_sandbox_seatbelt.rs:111`), and diagnostics. A simplified path is
strictly better in diagnostics too.

### Decision 4 — `dunce`, unconditional, not a hand-rolled strip

`dunce::canonicalize` is `fs::canonicalize` followed by `simplified`, which strips
`\\?\` **only when the result has a valid traditional spelling**. Its
`is_safe_to_strip_unc` refuses a `\\?\UNC\` share, a non-drive prefix, a path with
`.`/`..` components, and a path over 260 characters. That envelope is exactly the
one the ticket asks for ("only safe for paths that fit the traditional limits …
the failure mode there needs a decision rather than a silent truncation"), and it
is battle-tested rather than re-derived here.

Given Constraint 4, the supply-chain cost is zero.

**Unconditional, not `[target.'cfg(windows)'.dependencies]`.** `dunce::simplified`
is the identity function on non-Windows and `dunce::canonicalize` delegates to
`fs::canonicalize` there, so the crate is designed as a portable drop-in. Gating
it would buy a structurally-unchanged unix dependency tree at the cost of a
`#[cfg(windows)]`/`#[cfg(not(windows))]` pair around a single expression — two
code paths in a one-liner, which is how drift starts. Recorded as a deliberate
trade, not an oversight.

**Rejected: hand-rolled prefix strip.** ~40 lines plus the tests to prove the
260-character, reserved-name, and UNC-share edges, re-deriving a solved problem
that is already sitting in our lockfile.

### Decision 5 — an unfixable root degrades with a warning, never an error

`dunce::simplified` returns the input unchanged when there is no traditional
spelling. Two roots reach that state:

- **Longer than 260 characters.** No non-verbatim spelling exists.
- **On a network share.** `canonicalize` yields `\\?\UNC\server\share\…`, and the
  traditional spelling `\\server\share\…` is *itself* what `cmd.exe` rejects. No
  spelling fixes this case.

For both, cwd pinning still does not hold under `cmd.exe`. `Sandbox::open` must
**not** fail: that would break long-path and network-share users to satisfy a
documentation claim, on a code path where the FS tools continue to work correctly
(`cap-std` is unaffected) and only `BashTool`'s relative-path resolution is
degraded.

Instead, emit one `tracing::warn!` on the
`paigasus::tools::exec` target when the prefix survives on Windows, so an operator
seeing surprising relative-path resolution has a breadcrumb — mirroring the
degrade-path instrumentation SMA-613 established for the job object. Both cases
are documented (see "Documentation").

## Design

### Round 1 — `crates/paigasus-helikon-tools/tests/exec_cwd.rs` (new, ungated)

A new file rather than an addition to `exec_timeout_portable.rs`: that file is
about timeouts, and `exec_env_defaults.rs` already set the precedent that each
portable exec concern gets its own ungated file.

Two tests, both driving a real `HostBackend`:

**`host_backend_pins_cwd_to_the_sandbox_root`** — the diagnostic. Runs `pwd`
(unix) / `cd` (Windows), takes the **last** non-empty line of stdout, and
compares `canonicalize`d forms of the observed path and `sandbox.root()`.

- *Last* line, because a UNC banner — if `cmd.exe` prints one — precedes `cd`'s
  output. Taking the last line means the test fails on the *cwd*, not on the
  banner's presence.
- *Canonicalizing both sides* normalizes 8.3 short names (Constraint 3), case,
  and verbatim-ness, so this test cannot fail for a path-spelling reason. After
  Decision 3 lands, `root()` is `C:\...` and the observed path is `C:\...`;
  before it lands, both sides canonicalize to `\\?\C:\...`. Either way, equality
  means the cwd is genuinely the sandbox.
- The assertion message carries the full raw stdout. **That message is the
  experiment's readout** — it is how a red gate tells us `C:\Windows`.
- `exit_code == Some(0)` is asserted first, so a shell that failed to run at all
  is not read as a cwd verdict.

**`host_backend_resolves_a_relative_path_in_the_sandbox`** — the behavioural
twin, and the thing a user actually hits. Writes `marker.txt` into the sandbox,
runs `cat marker.txt` (unix) / `type marker.txt` (Windows), asserts exit 0 and
the file's content in stdout.

Not redundant with the first test: it fails with a different message, so a red
gate distinguishes "the cwd is somewhere else" from "the two spellings differ".
It is also the ungated counterpart of `host_backend_runs_command_in_cwd`, which
is stranded behind `#![cfg(unix)]`.

Neither test passes a path through the command string, so Constraint 2 does not
apply and neither needs the space-in-`TEMP` skip that
`timeout_kills_the_whole_subtree` carries.

**Drive-by correction.** `tests/exec_timeout_portable.rs`'s module doc claims to
be the only ungated real-process exec file in the crate. That was already false
when `exec_env_defaults.rs` landed on SMA-614; this file makes it a third. Fix
the comment in round 1.

### Round 2a — if confirmed

**`Cargo.toml` (root):** add `dunce = "1"` to `[workspace.dependencies]`.

**`crates/paigasus-helikon-tools/Cargo.toml`:** add `dunce = { workspace = true }`
to `[dependencies]`.

**`src/sandbox.rs`:** `root.canonicalize()` becomes `dunce::canonicalize(root)`,
error mapping unchanged. Add the Decision 5 warning, `#[cfg(windows)]`, when the
returned path still carries a verbatim prefix. Extend the `Sandbox::root()`
rustdoc with the Windows note and both residual limitations.

**`tests/sandbox.rs:16`:** update to compare against `dunce::canonicalize` so the
assertion stays true on Windows. `dunce` is already a dependency of the crate
under test, so it is in scope for the integration test without a dev-dependency.

### Round 2b — if refuted

Both tests stay as permanent regression guards. Add the comment the ticket asks
for at `src/sandbox.rs:35`, recording that a verbatim path is a fine `cmd.exe`
cwd — because `CreateProcessW`'s `lpCurrentDirectory` normalizes it before the
child observes it — and pointing at `tests/exec_cwd.rs` as the standing evidence.
The next reader will have the same doubt.

No `dunce`, no manifest change, no public behaviour change, and no doc change
beyond that comment: the three documented claims were already true.

## Testing

| Test | File | Gates it runs on |
| -- | -- | -- |
| `host_backend_pins_cwd_to_the_sandbox_root` | `tests/exec_cwd.rs` (new) | all six `test` matrix legs |
| `host_backend_resolves_a_relative_path_in_the_sandbox` | `tests/exec_cwd.rs` (new) | all six `test` matrix legs |
| `open_succeeds_on_existing_dir` (amended, 2a only) | `tests/sandbox.rs` | all six |

The Windows legs are the only ones that carry new information; the unix legs
guard Goal 4 (no behaviour change) — on unix `dunce::canonicalize` *is*
`fs::canonicalize`, so both new tests must be green locally on macOS before
round 1 is pushed.

### Local verification (macOS, before each push)

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test -p paigasus-helikon-tools
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
npx markdownlint-cli2                     # round 2a only (book page)
mdbook build docs/book                    # round 2a only
```

Local runs cannot answer the question. They only prove the change is green on the
platform that was never in doubt.

## Documentation

**Round 2a only.** Nothing below changes in round 2b.

- **`docs/book/src/concepts/tools.md`**, `HostBackend` section (line 379, "Pins
  the working directory to the sandbox root") — add the two residual Windows
  limitations from Decision 5: a root over 260 characters, and a root on a
  network share, keep the verbatim prefix and are not pinned. Must stay clean
  under `npx markdownlint-cli2` and `mdbook build docs/book`
  (`warning-policy = "error"`).
- **`Sandbox::root()` rustdoc** (`src/sandbox.rs`) — state that the path is the
  simplified canonical form on Windows and name the two exceptions.
- **`crates/paigasus-helikon-tools/README.md`** — reviewed and **no edit
  expected**. Its `HostBackend` mention is the one-line "cwd-pinned shell with env
  scrubbing and resource limits" in the feature roster; that claim becomes *more*
  true, and the platform caveat belongs in the book, not the crates.io landing
  page. Recorded as a conscious skip per CLAUDE.md, not a silent one.
- **No CHANGELOG edit** — release-plz generates it.

## Risks

| Risk | Mitigation |
| -- | -- |
| The Windows gate is red for an unrelated flake and is misread as confirmation | Decision 2: read the failure *output*, not the conclusion. The two tests fail with distinct, specific messages; anything else is a flake. |
| The gate is red and the message does not identify the observed cwd | The assertion embeds the full raw stdout, not just the parsed last line. |
| A third outcome: cwd is correct but the banner appears anyway | Taking the last non-empty line makes the test indifferent to the banner. Both tests pass; the fact is "refuted with a cosmetic banner", handled as round 2b plus a note. |
| `dunce` changes the unix build | `dunce::canonicalize` delegates to `fs::canonicalize` on non-Windows. The unix legs of the `test` matrix are the guard. |
| Round 1's draft PR is reviewed by CodeRabbit anyway | Harmless — the review lands on a state we are about to replace, and Stage 7 runs against the finished change. |
| A long-path or network-share root remains unpinned after the fix | Accepted and documented (Decision 5, Documentation). Erroring instead would break working setups. |
