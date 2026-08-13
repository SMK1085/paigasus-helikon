# SMA-458 — pin and checksum-verify `protoc` in CI

**Status:** approved
**Date:** 2026-08-13
**Linear:** [SMA-458](https://linear.app/smaschek/issue/SMA-458/ci-pin-an-explicit-protoc-version-in-setup-protoc-steps)
**Branch:** `feature/sma-458-ci-pin-an-explicit-protoc-version-in-setup-protoc-steps`

## Problem

SMA-332 added `arduino/setup-protoc` to every workflow that compiles the
workspace, SHA-pinned to v3.0.0 but with **no `version:` input**. The action
therefore resolves *the latest stable protoc release* at run time. A protoc
release can break CI days after any repo change, unrelated to that change and
un-pinned — while the rest of the repo pins SHAs and digests everywhere.

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

All 9 run `actions/checkout` **before** the protoc step, which is what makes a
repo-local composite action viable at every site.

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

## Decisions

**Exact version, not a wildcard.** `35.1`, which is what CI resolves today, so
this is a semantic no-op with zero validation risk. Since protobuf 4.x the
scheme is `MAJOR.MINOR`, so `35.x` would admit *minor* releases carrying real
feature changes — not patch-only, as the ticket's "pin the major" framing
implies. The ticket's literal `31.x` example is four majors stale and would be
an unvalidated downgrade from the version CI has been proving green.

**One composite action, not 9 inline blocks.** With three platform digests in
play, repeating the install at 9 sites would mean 9 copies to keep in sync —
precisely the drift this ticket exists to prevent.

**`arduino/setup-protoc` leaves the tree.** Verifying *before* extracting
requires owning the download. Wrapping the third-party action instead would
verify only after it had already extracted the artifact, and would pin binary
digests rather than the published archive digests.

## Design

### `.github/actions/setup-protoc/action.yml`

A composite action with **no inputs** — a `version` input would defeat the point
of a single pinned version. It carries `PROTOC_VERSION` and the three archive
digests, and per platform performs, strictly in this order:

**download → verify → extract → PATH**

The order is the whole reason for this approach: an unverified binary never
reaches an executable location and is never run.

Digests, computed from the published v35.1 assets:

| asset | SHA-256 |
| --- | --- |
| `protoc-35.1-linux-x86_64.zip` | `6930ebf62bd4ea607b98fff052596c6ee564b9835b4ce172c75a3f53ae9d91b7` |
| `protoc-35.1-osx-aarch_64.zip` | `193289af0470c6a1aada357d4fba0bbf8d78bfaac8b5e42ca30af2ef75583de2` |
| `protoc-35.1-win64.zip` | `5d3ff218d7d91eea95f7569bcb5a98f3030f8996d44151279d9772edcff76082` |

### Platform table

| `RUNNER_OS`-`RUNNER_ARCH` | asset | exercised by |
| --- | --- | --- |
| `Linux-X64` | `linux-x86_64` | 8 of the 9 sites |
| `macOS-ARM64` | `osx-aarch_64` | `ci.yml` `test` matrix |
| `Windows-X64` | `win64` | `ci.yml` `test` matrix |

Any other combination **fails loudly**, with an error naming this file as the
place to add support.

`linux-aarch_64` is deliberately **not** pre-added, even though the repo runs
`ubuntu-24.04-arm` for `agentcore-image` (which has no protoc step). An unused
digest is a code path CI never exercises: if it were wrong, the first user would
hit a checksum mismatch that reads as tampering rather than as a typo. An
explicit "unsupported platform" error is unambiguous. Adding it is a two-line
change if a protoc-needing arm64 job ever appears.

### Correctness traps

These are the three things most likely to be got wrong, recorded so the plan
treats each as real work:

1. **`include/` must survive extraction.** Each archive contains `bin/protoc`
   **and** `include/google/protobuf/*.proto` — the well-known types
   (`descriptor`, `timestamp`, `duration`, …) that the temporal protos import.
   `protoc` resolves them relative to its own binary, so extraction must
   preserve the `bin/` + `include/` sibling layout and `PATH` must point at
   `<root>/bin`.

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

   So an install that drops the bare binary on `PATH` fails at the first
   well-known-type import — which is every temporal proto.

2. **`sha256sum` does not exist on macOS.** It is present on Linux runners and
   in Git Bash on Windows; macOS provides `shasum -a 256`. Both accept `-c -`,
   so a single `command -v sha256sum` shim covers all three platforms.

3. **Windows is its own task, not an afterthought.** `unzip` is not guaranteed
   in Git Bash, and `$RUNNER_TEMP` on Windows is a backslash path that Git Bash
   mangles (`mkdir -p "D:\a\_temp\protoc"` creates one literal file, not a
   directory tree). This is the most likely part to need a CI round-trip to get
   right.

### Never green by skip

The action asserts `protoc --version` reports `libprotoc 35.1` and fails the
step otherwise. The expected string is verified against the real v35.1 binary,
not assumed:

```console
$ ./extracted/bin/protoc --version
libprotoc 35.1
```

This is load-bearing, not decoration. Ubuntu runners may carry a system
`protoc`; if the `PATH` prepend silently failed, the workspace would compile
against the *wrong* compiler and the job would look identical to a correct one.
The assertion is the only thing that distinguishes them — the same reasoning as
`HELIKON_REQUIRE_TEMPORAL` in `integration.yml` and `HELIKON_REQUIRE_SANDBOX` in
`ci.yml`.

### Call sites

Each of the 9 becomes:

```yaml
- uses: ./.github/actions/setup-protoc
```

Existing `if:` guards are preserved verbatim on the two guarded sites
(`sessions-it`, `temporal-it`). Each site's multi-line explanatory comment
collapses to a pointer at the action, which is where the reasoning now lives.

## Verification

CI is the test; there is no local harness for a GitHub composite action.

- `test (macos-latest, stable)` and `test (windows-latest, stable)` are the only
  gates exercising the two non-Linux branches. Both are already **required**
  contexts, so neither can regress unnoticed.
- The other 7 sites are covered by their own jobs going green.
- `release-plz.yml` is one of the 9. If the action is broken there, releases
  break — but it fails closed (checksum mismatch or version assertion), never
  silently.
- Locally: `actionlint .github/workflows/*.yml` must stay at its **one
  pre-existing finding** (`ci.yml:228`, SC2034 — the unused `i` in
  `sessions-it`'s retry loop). No new findings.

## Documentation

- **CLAUDE.md** — CI section: the pin, the hand-bump duty (Dependabot sees the
  action SHA, never a pinned artifact version, and there is no longer a
  third-party action here to track at all), why only three platforms are
  supported, and why the version assertion exists.
- **CONTRIBUTING.md** — "Build prerequisites" already tells contributors to
  install `protoc`; name 35.1 so a local install can match CI. Distribution
  packages are frequently many majors behind, which is a real source of
  "green locally, red in CI".

## Non-goals

- **mdBook and crate READMEs.** Pure-internal CI change with no user-facing
  surface — a conscious skip under the CLAUDE.md rule, not an oversight.
- **Provenance verification.** Out of reach: protobuf publishes no attestation
  for the protoc binary archives. See "What a pinned digest actually buys".
- **Caching the download.** `arduino/setup-protoc` did not cache either, so this
  is not a regression; adding it would be new scope.
- **A `version` input on the action.** Deliberately omitted (YAGNI).
