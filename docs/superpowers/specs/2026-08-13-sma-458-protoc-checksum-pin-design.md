# SMA-458 — pin and checksum-verify `protoc` in CI

**Status:** approved (revised after adversarial challenge)
**Date:** 2026-08-13
**Linear:** [SMA-458](https://linear.app/smaschek/issue/SMA-458/ci-pin-an-explicit-protoc-version-in-setup-protoc-steps)
**Branch:** `feature/sma-458-ci-pin-an-explicit-protoc-version-in-setup-protoc-steps`

## Problem

SMA-332 added `arduino/setup-protoc` to every workflow that compiles the
workspace, SHA-pinned to v3.0.0 but with **no `version:` input**.

**The ticket's stated premise is wrong, and this was checked rather than
assumed.** The ticket asserts the action "defaults to the latest protoc
release." It does not. Its `action.yml` at the pinned SHA declares:

```yaml
inputs:
  version:
    description: 'Version to use. Example: 23.2'
    default: '23.x'
```

The action's README claims otherwise ("To get the latest stable version of
`protoc` just add this step") — the README is wrong, the `action.yml` is
authoritative. CI confirms it. From `ci.yml` run `31697089240` on `main`
(2026-08-13), identically across `clippy`, `docs`, `doc-coverage`,
`sessions-it`, and `test (windows-latest, stable)`:

```
Getting protoc version: v23.4
Downloading archive: https://github.com/protocolbuffers/protobuf/releases/download/v23.4/protoc-23.4-linux-x86_64.zip
```

**So CI has been running protoc 23.4, and 23.x is EOL** — 23.4 was the last of
that line, so the `23.x` wildcard is already frozen in practice. The
"a future protoc release could break CI days later" risk the ticket describes
therefore does not exist as stated.

**The real residual risk is narrower, and still worth closing.** The repo
implicitly depends on a *third-party action's default value*. A future
`arduino/setup-protoc` release could change that default and silently move the
compiler — and Dependabot's `github-actions` group would deliver exactly such a
bump. Alongside that, the download has no integrity verification at all. Those
two are what this change closes.

The ticket describes 8 sites. **There are 9**: `integration.yml` gained one in
SMA-457, after the ticket was filed.

| Workflow | Job | Line | Runner |
| --- | --- | --- | --- |
| `ci.yml` | `clippy` | 56 | `ubuntu-latest` |
| `ci.yml` | `test` | 90 | `${{ matrix.os }}` — ubuntu / macos / windows |
| `ci.yml` | `build-no-default-features` | 110 | `ubuntu-latest` |
| `ci.yml` | `docs` | 145 | `ubuntu-latest` |
| `ci.yml` | `doc-coverage` | 169 | `ubuntu-latest` |
| `ci.yml` | `sessions-it` | 220 | `ubuntu-latest` (`if:`-guarded) |
| `msrv.yml` | `verify` | 31 | `ubuntu-latest` |
| `release-plz.yml` | `release-plz` | 35 | `ubuntu-latest` |
| `integration.yml` | `temporal-it` | 121 | `ubuntu-latest` (`if:`-guarded) |

All 9 run `actions/checkout` **before** the protoc step, which is the
precondition that makes a repo-local composite action viable at every site.

`bench.yml` is **deliberately not** in this list, and this was checked rather
than assumed: it runs `cargo bench -p paigasus-helikon --bench tool_dispatch`,
and the facade's `default = []` pulls in no `runtime-temporal`, so nothing in
that job compiles a `.proto`.

## Scope decision

The ticket scopes this as a one-line `version:` input. During design the scope
was deliberately widened to include **checksum verification**, because pinning a
version alone closes only half the hole it describes:

- *Version drift* — a new protoc release changes CI behaviour unannounced.
  Closed by pinning a version.
- *Artifact integrity* — the binary is fetched from GitHub releases with no
  verification at all. **Not** closed by pinning a version, and the reason the
  Temporal CLI install step in `integration.yml` already runs `sha256sum -c`.

Widening the scope is recorded here rather than left implicit: this is no longer
a one-line change, and the ticket's Low priority reflects its original framing.

### What a pinned digest actually buys

protobuf publishes SLSA provenance (`.intoto.jsonl`) **only for the bazel source
tarball**, not for the `protoc` binary zips. A pinned digest is therefore
trust-on-first-use: it converts "trust GitHub on every run" into "trust GitHub
once, at pin time." That catches later tampering or a silent re-tag; it is *not*
provenance verification, and this spec does not claim otherwise. It is the same
footing as the existing `TEMPORAL_CLI_SHA256`, and the same reasoning applies —
a checksum served by the artifact's own host would prove only that the download
was not corrupted, never the artifact's identity.

**The guarantee applies to trusted refs only.** On `pull_request` the composite
action is checked out from the PR head, so a fork PR can edit the digests in the
same commit. This is not a regression — the whole tree already executes — but
the pin defends `main` and the release path, not arbitrary PR code.

### The availability cost, accepted explicitly

Pinning is a trade, not pure upside. `arduino/setup-protoc` resolving "latest"
self-heals if an upstream asset is deleted or re-tagged; a pinned URL + digest
does not. If protobuf removes or replaces the v35.1 assets, **every required CI
job and `release-plz` go red at once and stay red** until a human bumps the
version and all three digests. The mitigation is the bump runbook (below) being
written down *before* it is needed, not the risk being avoided.

## Decisions

**Exact version, not a wildcard.** `35.1`. Since protobuf 4.x the scheme is
`MAJOR.MINOR`, so `35.x` would admit *minor* releases carrying real feature
changes — not patch-only, as the ticket's "pin the major" framing implies. The
ticket's literal `31.x` example is four majors stale and would be an unvalidated
downgrade.

**This is a deliberate upgrade, not a no-op.** An earlier draft of this spec
claimed `35.1` was "what CI resolves today … a semantic no-op with zero
validation risk." That was wrong — CI runs 23.4 (see Problem). Pinning 35.1 is a
**12-major upgrade**, 23.4 → 35.1, and this PR carries it.

Evidence that the upgrade is viable, gathered before the decision:

```console
$ PROTOC=…/protoc-35.1/bin/protoc PROTOC_INCLUDE=…/protoc-35.1/include \
    cargo check -p paigasus-helikon-runtime-temporal
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 35.47s
```

That is the workspace's heaviest protoc consumer building clean on 35.1 — but on
**macOS-arm64 only**. Linux and Windows remain unproven until CI runs.

**The approver accepted a known trade-off here.** Combining the mechanism change
with the compiler upgrade means a red CI job is ambiguous between the two, and
the two least-gated surfaces — Windows (no merge gate) and `release-plz.yml`
(never exercised pre-merge) — are exactly where that ambiguity is most
expensive. The alternative (pin 23.4 now, upgrade in a follow-up) was offered and
declined in favour of one PR.

**Mitigation: bisecting is a two-line edit, because the 23.4 digests are banked
here.** To isolate mechanism from compiler, set `PROTOC_VERSION="23.4"` and swap
in:

| asset | protoc 23.4 SHA-256 |
| --- | --- |
| `protoc-23.4-linux-x86_64.zip` | `0502f286ac9ed860b629a7965a14527b1f2dd131e4283fa23c2d7f184672aa9a` |
| `protoc-23.4-osx-aarch_64.zip` | `8c7afae8626b6811e7b5897d16d940c2dbf50b1e135ed958a01db6566bdda726` |
| `protoc-23.4-win64.zip` | `a309c39442fb75f0db343cb22c111a00f91cdf0767f332e170644b9378e2bcc6` |

Green on 23.4 and red on 35.1 means the compiler; red on both means the
mechanism. Derived the same way and on the same date as the 35.1 digests below.

**One composite action, not 9 inline blocks.** With three platform digests in
play, repeating the install at 9 sites would mean 9 copies to keep in sync —
precisely the drift this ticket exists to prevent.

**`arduino/setup-protoc` leaves the tree.** Verifying *before* extracting
requires owning the download. Wrapping the third-party action instead would
verify only after it had already extracted the artifact, and would pin binary
digests rather than the published archive digests.

**No `version` input** — it would defeat the point of a single pinned version.
This is narrower than "no inputs": the `repo-token` question is separate and is
answered below on its own merits.

**No token, and not for lack of a way to pass one.** `arduino/setup-protoc`
needed `repo-token` because it queries the GitHub *releases API* to resolve
"latest" or a semver range. This design constructs the asset URL directly from a
hardcoded version, so it makes **no API call at all** and is not subject to the
60/hr unauthenticated API limit. Release-asset downloads redirect to
`objects.githubusercontent.com`. Dropping `repo-token` is therefore not a
regression; the remedy for transient failure is retries, specified below.

## Design

### `.github/actions/setup-protoc/action.yml` + `install.sh`

The install logic lives in **`.github/actions/setup-protoc/install.sh`**, and
`action.yml` invokes it as:

```yaml
- shell: bash
  run: bash "${GITHUB_ACTION_PATH}/install.sh"
```

A separate script rather than inline YAML shell, for one reason that decides it:
inline shell in `action.yml` is lintable and runnable by nothing, whereas a
script is `shellcheck`-clean-able and can be dry-run locally (see Verification).
`$GITHUB_ACTION_PATH` is what resolves the script's location inside a composite
action; a workspace-relative path is wrong.

**Every `run:` step in a composite action must declare `shell:`** — composite
actions have no default shell, and the runner rejects the action outright
without it.

Per platform, strictly in this order:

**download → verify → extract → export**

The order is the whole reason for this approach: an unverified binary never
reaches an executable location and is never run.

### Environment export, not just PATH

The action writes to `$GITHUB_ENV`:

```
PROTOC=<root>/bin/protoc[.exe]
PROTOC_INCLUDE=<root>/include
```

and additionally prepends `<root>/bin` to `$GITHUB_PATH`.

`PROTOC` is what makes the install **authoritative**: `prost-build` (0.14.4 in
`Cargo.lock`) resolves `PROTOC` from the environment *before* falling back to a
`PATH` lookup, so exporting it removes the PATH-ordering question entirely and
makes trap #1's relative-`include` resolution moot rather than merely handled.
`CONTRIBUTING.md:158` already documents `PROTOC` for local builds, so this is
the mechanism the repo already tells contributors to use.

### Download

```
https://github.com/protocolbuffers/protobuf/releases/download/v${VERSION}/protoc-${VERSION}-${ASSET}.zip
```

Note the asymmetry that would otherwise cost a confusing 404: the **tag** is
`v35.1` (with `v`), the **filename** is `protoc-35.1-…` (without).

```bash
curl -sSfL --retry 3 --retry-all-errors --retry-delay 2 \
     --connect-timeout 15 --max-time 300 -o "$zip" "$url"
```

Retries are not optional decoration here. This install runs in **11 job
executions per PR** (the 6-row `test` matrix plus `clippy`,
`build-no-default-features`, `docs`, `doc-coverage`, `sessions-it`), nine of
which back required contexts, plus the crates.io publish path. The bare
`curl -sSfL` modelled by `integration.yml:135` is tolerable there precisely
because `temporal-it` is signal-only.

### Digests

Derived on 2026-08-13 by downloading each asset from the release page
<https://github.com/protocolbuffers/protobuf/releases/tag/v35.1> and running
`shasum -a 256`:

| asset | SHA-256 |
| --- | --- |
| `protoc-35.1-linux-x86_64.zip` | `6930ebf62bd4ea607b98fff052596c6ee564b9835b4ce172c75a3f53ae9d91b7` |
| `protoc-35.1-osx-aarch_64.zip` | `193289af0470c6a1aada357d4fba0bbf8d78bfaac8b5e42ca30af2ef75583de2` |
| `protoc-35.1-win64.zip` | `5d3ff218d7d91eea95f7569bcb5a98f3030f8996d44151279d9772edcff76082` |

**The reviewer must independently recompute at least one of these.** A digest
nobody re-derived is a number, not a control — and this spec's own argument for
omitting `linux-aarch_64` (that a wrong digest reads as tampering rather than as
a typo) applies with equal force to the three it ships.

### On a checksum mismatch

The action fails with an explicit `::error::` naming the three possible causes —
truncated or corrupted download, upstream re-tag or asset replacement, or
tampering — and stating: **do not update the digest without independently
verifying the upstream release.** Without this, the natural reflex on a red
build is to paste in the new number, which converts a tamper alarm into a rubber
stamp.

### Platform table

| `RUNNER_OS`-`RUNNER_ARCH` | asset | binary | exercised by |
| --- | --- | --- | --- |
| `Linux-X64` | `linux-x86_64` | `bin/protoc` | 8 of the 9 sites |
| `macOS-ARM64` | `osx-aarch_64` | `bin/protoc` | `ci.yml` `test` matrix |
| `Windows-X64` | `win64` | `bin/protoc.exe` | `ci.yml` `test` matrix |

Any other combination **exits non-zero** with an error naming this file as the
place to add support.

`linux-aarch_64` is deliberately **not** pre-added, even though the repo runs
`ubuntu-24.04-arm` for `agentcore-image` (which has no protoc step). An unused
digest is a code path CI never exercises: if it were wrong, the first user would
hit a checksum mismatch that reads as tampering rather than as a typo. Adding it
is a two-line change if a protoc-needing arm64 job ever appears.

### Correctness traps

1. **`include/` must survive extraction.** Each archive contains `bin/protoc`
   **and** `include/google/protobuf/*.proto` — the well-known types
   (`descriptor`, `timestamp`, `duration`, …) that the temporal protos import.
   `protoc` resolves them relative to its own binary, so extraction must
   preserve the `bin/` + `include/` sibling layout.

   **Confirmed against the real v35.1 binary, not inferred.** With the sibling
   tree intact, a proto importing `google/protobuf/timestamp.proto` compiles
   with no `-I` pointing at `include/`:

   ```console
   $ ./extracted/bin/protoc --proto_path=. --descriptor_set_out=/dev/null t.proto
   $ echo $?
   0
   ```

   With the bare binary copied away from its `include/` sibling, the identical
   command fails:

   ```console
   $ ./bareonly/protoc --proto_path=. --descriptor_set_out=/dev/null t.proto
   google/protobuf/timestamp.proto: File not found.
   t.proto:2:1: Import "google/protobuf/timestamp.proto" was not found or had errors.
   t.proto:3:13: "google.protobuf.Timestamp" is not defined.
   $ echo $?
   1
   ```

   Exporting `PROTOC_INCLUDE` (above) makes this belt-and-braces rather than the
   sole defence.

2. **`sha256sum` does not exist on macOS.** Present on Linux runners and in Git
   Bash on Windows; macOS provides `shasum -a 256`. Both accept `-c -`, so a
   single `command -v sha256sum` shim covers all three platforms.

3. **Extraction tool and the executable bit.** `unzip` preserves the archive's
   Unix mode; PowerShell `Expand-Archive` and Python `zipfile` **do not**,
   yielding a non-executable `protoc` whose `Permission denied` surfaces
   mid-`cargo build` inside a build script, far from the install step. Decided
   here rather than at implementation time:

   - **Linux / macOS** — `unzip -q`, followed by an unconditional
     `chmod +x "$root/bin/protoc"`.
   - **Windows** — `7z x` (present on the runner image; `unzip` is *not*
     guaranteed in Git Bash). The executable bit is irrelevant on Windows.

4. **Windows path handling.** Two concrete hazards, replacing the earlier
   unverified claim about `mkdir` and backslash paths:

   - **`$GITHUB_PATH` must receive a Win32 path.** A Git-Bash-style
     `/d/a/_temp/…` entry looks fine inside bash but is a literal, useless
     `PATH` entry to `CreateProcess` when cargo spawns protoc. Use
     `cygpath -w` on what is written to `$GITHUB_PATH` and `$GITHUB_ENV`, and
     `cygpath -u` when consuming `$RUNNER_TEMP` inside bash. This is the most
     likely Windows failure mode, and it is invisible to a bash-side check.
   - **`protoc --version` can emit a trailing `\r`**, so a bare
     `[ "$(protoc --version)" = "libprotoc 35.1" ]` fails on a *correct*
     install. Strip with `tr -d '\r'`.

### Never green by skip

The version assertion must be a **separate composite step** from the step that
writes `$GITHUB_PATH`/`$GITHUB_ENV`, and must check **both**:

1. `protoc --version` (`\r`-stripped) equals `libprotoc 35.1`; and
2. `command -v protoc` resolves to the expected absolute path.

Both halves are load-bearing, and the earlier draft of this spec got this wrong
in a way worth recording. `$GITHUB_PATH` and `$GITHUB_ENV` writes **do not
affect the step that makes them** — only later steps. An assertion sharing a
step with the export therefore validates a local `export PATH=`, not the
mechanism later steps actually use, and is structurally blind to the
PATH-propagation failure it exists to catch. And a version check alone passes if
some *other* 35.1 protoc is first on `PATH`, which is why the resolved path is
checked too.

The expected string is verified against the real v35.1 binary:

```console
$ ./extracted/bin/protoc --version
libprotoc 35.1
```

The justification is **not** "ubuntu runners may carry a system protoc" — that
was asserted in the earlier draft without evidence and is not relied upon. The
failures this actually catches are: extraction that silently produced no binary,
a lost executable bit, a Win32-vs-POSIX `$GITHUB_PATH` entry, and `.exe`
resolution on Windows.

### Observability

On success the action logs four things, because they are what makes a future
failure diagnosable in one look: the selected asset, the download URL, the
computed digest alongside the expected one, and the final resolved
`command -v protoc`.

### Call sites

Each of the 9 becomes:

```yaml
- uses: ./.github/actions/setup-protoc
```

Existing `if:` guards are preserved verbatim on the two guarded sites
(`sessions-it`, `temporal-it`). Each site's multi-line explanatory comment
collapses to a pointer at the action, which is where the reasoning now lives.

**Both path filters must gain `.github/actions/**`.** `sessions-it`
(`ci.yml:204-210`) filters on `.github/workflows/ci.yml`; `temporal-it`
(`integration.yml:88-95`) filters on `.github/workflows/integration.yml`. This
change moves the install logic *out* of those files, so without the addition the
next protoc bump — a PR touching only `.github/actions/setup-protoc/**` — skips
both jobs, including `sessions-it`, a **required** check, which would report
green having run nothing protoc-related. That is exactly the never-green-by-skip
class this spec is otherwise careful about, and it is a regression this change
would introduce.

## Verification

### Local (this is not a CI-only change)

The earlier draft claimed "there is no local harness." That was false, and it
had shaped the design toward the least testable shape available. Splitting out
`install.sh` makes the new logic locally checkable:

- `shellcheck .github/actions/setup-protoc/install.sh` — clean. Note that
  `actionlint` lints `.github/workflows/*.yml` and **not** `action.yml`, so
  without this the new logic has *zero* lint coverage.
- Dry run on Linux and on macOS-ARM64 with the runner contract faked:
  ```bash
  RUNNER_OS=Linux RUNNER_ARCH=X64 RUNNER_TEMP=$(mktemp -d) \
    GITHUB_PATH=$(mktemp) GITHUB_ENV=$(mktemp) \
    bash .github/actions/setup-protoc/install.sh
  ```
  The dev host is arm64 macOS, so the `macOS-ARM64` branch is directly
  exercisable locally — it is not CI-only.
- Independent recomputation of at least one digest.
- `actionlint .github/workflows/*.yml` must introduce no new findings. The
  baseline is **one** pre-existing finding: SC2034 for the unused `i` in
  `sessions-it`'s readiness loop. It is described by content, not line number,
  because removing six `with: repo-token:` blocks shifts every later line in
  `ci.yml`.

### CI

- `test (macos-latest, stable)` is a **required** context and gates the macOS
  branch.
- **`test (windows-latest, stable)` is NOT required** —
  `.github/rulesets/main-protection-checks.json` lists only
  `test (ubuntu-latest, stable)` and `test (macos-latest, stable)`; CLAUDE.md
  records the Windows and `1.94` matrix rows as signals only. The earlier draft
  claimed otherwise, and the error mattered: Windows is the branch this spec
  identifies as riskiest and it is the one with **no merge gate**. A red Windows
  job can merge to `main`, and `release-plz.yml` is the next thing to touch
  protoc.

  **Mitigation:** the implementer must manually confirm
  `test (windows-latest, stable)` green before requesting merge, as an explicit
  PR-checklist item. Promoting it to a required context is a repo-policy change
  affecting every future PR and is deliberately **not** bundled here — flagged
  for the approver as a possible follow-up ticket.

### `release-plz.yml` — blast radius, stated honestly

`release-plz.yml` triggers on `push: branches: [main]` only. **The change to it
is never exercised by the PR**; its first execution is post-merge against a live
crates.io token.

The earlier draft claimed it "fails closed, never silently." That is wrong, and
the repo's own history says so: in SMA-332 a missing protoc in this workflow
produced a **partial release — 3 of 5 crates published**. release-plz publishes
in dependency order, so a mid-run failure leaves crates.io partially published,
and publishing is irreversible.

What is true: the Linux branch here is byte-identical to the 8 other Linux
sites, all of which the PR does exercise, so the marginal risk is a *transient
download failure at release time* rather than a logic error. That is what the
`--retry` flags above are for.

**Recovery:** re-run the workflow. release-plz skips already-published versions,
so a partial release is completed rather than corrupted by a re-run.

**Rollback:** revert the PR. Because the action is additive and self-contained,
reverting restores `arduino/setup-protoc` at all 9 sites in one commit.

## Documentation

- **CLAUDE.md** — CI section: the pin, the hand-bump duty, why only three
  platforms, and why the version assertion exists. State plainly that
  **Dependabot cannot see this** — it tracks action SHAs, and after this change
  there is no third-party action here to track at all. List it alongside the
  repo's other hand-bumped pins (`TEMPORAL_CLI_VERSION` / `TEMPORAL_CLI_SHA256`,
  `NIGHTLY_TOOLCHAIN`) so all of them are discoverable in one place.
- **CONTRIBUTING.md** — two edits, not one:
  - Line 158 becomes **factually false** on merge ("CI installs it via
    `arduino/setup-protoc` in every job that compiles the workspace") and must
    be rewritten to name the local composite action.
  - Name protoc 35.1 so a local install can match CI, and point at the release
    page for a matching binary — `apt-get install protobuf-compiler` on Ubuntu
    24.04 is many majors behind, so "match CI" is not actionable via the distro
    package alone.
- **Bump runbook** (in CLAUDE.md, alongside the pin): which files to edit, how
  to derive the three digests, and how to verify — because this is also the
  recovery procedure for the availability risk named above, and the answer to
  "who bumps this, on what trigger" is *a human, with no automated prompt*.

## Commit and PR conventions

`.versionrc`'s `scopeRegex` has **no `actions` scope**, so `ci(actions):` would
fail the local `commit-msg` hook, the `commits` job, and the required `pr-title`
check. Use **`ci(workflows): SMA-458 …`** with a lowercase subject.

## Non-goals

- **mdBook and crate READMEs.** Pure-internal CI change with no user-facing
  surface — a conscious skip under the CLAUDE.md rule, not an oversight.
- **Provenance verification.** Out of reach: protobuf publishes no attestation
  for the protoc binary archives.
- **Caching the download.** `arduino/setup-protoc` provides no cross-run caching
  on ephemeral hosted runners, so this is not a regression; adding caching would
  be new scope.
- **Promoting `test (windows-latest, stable)` to a required context.** Discussed
  under Verification; a repo-policy change, deliberately not bundled.
