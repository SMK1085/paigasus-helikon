# SMA-458 protoc Pin and Checksum Verification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `arduino/setup-protoc` at all 9 call sites with a repo-local composite action that installs a pinned, checksum-verified protoc 35.1.

**Architecture:** One composite action at `.github/actions/setup-protoc/` containing three scripts — `install.sh` (download → verify → extract → export), `verify.sh` (assert the pinned protoc is what later steps will use, in its own step), and `selftest.sh` (local harness; not run by CI). All 9 workflow sites reduce to `uses: ./.github/actions/setup-protoc`.

**Tech Stack:** GitHub Actions composite actions, bash, `curl`, `unzip`/`7z`, `sha256sum`/`shasum`, `cygpath`.

**Spec:** `docs/superpowers/specs/2026-08-13-sma-458-protoc-checksum-pin-design.md`

## Global Constraints

Every task's requirements implicitly include this section.

- **protoc version is `35.1`** — exact, no wildcard. This is a deliberate 12-major upgrade from the 23.4 CI has actually been running.
- **The three pinned SHA-256 digests are, verbatim:**
  - `linux-x86_64` → `6930ebf62bd4ea607b98fff052596c6ee564b9835b4ce172c75a3f53ae9d91b7`
  - `osx-aarch_64` → `193289af0470c6a1aada357d4fba0bbf8d78bfaac8b5e42ca30af2ef75583de2`
  - `win64` → `5d3ff218d7d91eea95f7569bcb5a98f3030f8996d44151279d9772edcff76082`
- **Order is load-bearing:** download → verify → extract → export. An unverified archive must never reach an executable location.
- **Commit prefix is `ci(workflows): SMA-458 <lowercase subject>`.** `.versionrc`'s `scopeRegex` has **no `actions` scope** — `ci(actions):` fails the local `commit-msg` hook, the `commits` CI job, and the required `pr-title` check.
- **Every `run:` step in a composite action must declare `shell:`.** Composite actions have no default shell; the runner rejects the action outright without it.
- **Do not run `git add -A`.** `.env` and `.claude/` are untracked but *not* gitignored. Stage explicit paths only.
- **Run `actionlint .github/workflows/*.yml` before each commit.** Baseline is exactly **one** pre-existing finding: SC2034 for the unused `i` in `sessions-it`'s readiness loop. Introduce no others. (Described by content, not line number — this change shifts line numbers in `ci.yml`.)

---

### Task 1: The composite action

**Files:**
- Create: `.github/actions/setup-protoc/selftest.sh`
- Create: `.github/actions/setup-protoc/install.sh`
- Create: `.github/actions/setup-protoc/verify.sh`
- Create: `.github/actions/setup-protoc/action.yml`

**Interfaces:**
- Consumes: the GitHub Actions runner contract — `RUNNER_OS`, `RUNNER_ARCH`, `RUNNER_TEMP`, `GITHUB_PATH`, `GITHUB_ENV`, `GITHUB_ACTION_PATH`.
- Produces: an action referenced by later tasks as `uses: ./.github/actions/setup-protoc`. It exports `PROTOC` (absolute path to the binary) and `PROTOC_INCLUDE` (absolute path to the well-known-type tree) via `$GITHUB_ENV`, and prepends `<root>/bin` to `$GITHUB_PATH`.

- [ ] **Step 1: Write the failing test**

Create `.github/actions/setup-protoc/selftest.sh`:

```bash
#!/usr/bin/env bash
# Local self-test for the setup-protoc composite action.
#
# NOT run by CI — in CI the action itself is the test. This exists so the install
# logic can be exercised, and a version bump re-verified, without pushing a
# commit. Run it after changing PROTOC_VERSION or any digest.
#
#   bash .github/actions/setup-protoc/selftest.sh
#
# Requires network access. The Windows branch of install.sh cannot be exercised
# here (it needs cygpath and 7z) — CI's `test (windows-latest, stable)` is the
# only thing covering it, and that job is NOT a required context, so confirm it
# by hand before merging.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
failures=0
pass() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; failures=$((failures + 1)); }

sha256_of() {
  if command -v sha256sum > /dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) native_os=macOS; native_arch=ARM64 ;;
  Linux-x86_64) native_os=Linux; native_arch=X64   ;;
  *) echo "selftest: no native mapping for $(uname -s)-$(uname -m)"; exit 1 ;;
esac

version="$(awk -F'"' '/^PROTOC_VERSION=/ {print $2}' "${here}/install.sh")"
echo "selftest: install.sh pins protoc ${version}"

# --- 1. every pinned digest still matches the published asset ----------------
echo "1. pinned digests match published assets"
while read -r var asset; do
  expected="$(awk -F'"' -v pat="^${var}=" '$0 ~ pat {print $2}' "${here}/install.sh")"
  tmp="$(mktemp -d)"
  url="https://github.com/protocolbuffers/protobuf/releases/download/v${version}/protoc-${version}-${asset}.zip"
  if curl -sSfL --retry 3 -o "${tmp}/a.zip" "${url}"; then
    actual="$(sha256_of "${tmp}/a.zip")"
    if [ "${actual}" = "${expected}" ]; then
      pass "${asset}"
    else
      fail "${asset}: pinned ${expected}, published ${actual}"
    fi
  else
    fail "${asset}: download failed (${url})"
  fi
  rm -rf "${tmp}"
done <<'ASSETS'
SHA256_LINUX_X64 linux-x86_64
SHA256_MACOS_ARM64 osx-aarch_64
SHA256_WINDOWS_X64 win64
ASSETS

# --- 2. native install, then the real verify.sh must pass --------------------
echo "2. native install + verify (${native_os}-${native_arch})"
t2="$(mktemp -d)"
if env RUNNER_OS="${native_os}" RUNNER_ARCH="${native_arch}" RUNNER_TEMP="${t2}" \
       GITHUB_PATH="${t2}/gh_path" GITHUB_ENV="${t2}/gh_env" \
       bash "${here}/install.sh" > "${t2}/install.log" 2>&1; then
  pass "install.sh exited 0"
else
  fail "install.sh failed"; sed 's/^/      /' "${t2}/install.log"
fi

# Reproduce what the runner does between steps: GITHUB_ENV lines become the next
# step's environment, GITHUB_PATH lines are prepended to PATH.
if [ -s "${t2}/gh_env" ] && [ -s "${t2}/gh_path" ]; then
  set -a; . "${t2}/gh_env"; set +a
  PATH="$(head -n 1 "${t2}/gh_path"):${PATH}"; export PATH
  if env RUNNER_OS="${native_os}" bash "${here}/verify.sh" > "${t2}/verify.log" 2>&1; then
    pass "verify.sh exited 0"
  else
    fail "verify.sh failed"; sed 's/^/      /' "${t2}/verify.log"
  fi
else
  fail "install.sh exported nothing to GITHUB_ENV/GITHUB_PATH"
fi

# --- 3. cross-platform install lays down bin/ + include/ ---------------------
# Exercises a non-native digest and extraction path. The binary is not executed.
if [ "${native_os}" != "Linux" ]; then
  echo "3. cross-platform install (Linux-X64)"
  t3="$(mktemp -d)"
  if env RUNNER_OS=Linux RUNNER_ARCH=X64 RUNNER_TEMP="${t3}" \
         GITHUB_PATH="${t3}/gh_path" GITHUB_ENV="${t3}/gh_env" \
         bash "${here}/install.sh" > "${t3}/install.log" 2>&1; then
    pass "install.sh exited 0"
  else
    fail "install.sh failed"; sed 's/^/      /' "${t3}/install.log"
  fi
  [ -f "${t3}/protoc-${version}/bin/protoc" ] \
    && pass "bin/protoc present" || fail "bin/protoc missing"
  [ -f "${t3}/protoc-${version}/include/google/protobuf/timestamp.proto" ] \
    && pass "include tree present" || fail "include tree missing"
  [ -x "${t3}/protoc-${version}/bin/protoc" ] \
    && pass "executable bit set" || fail "executable bit not set"
else
  echo "3. cross-platform install — skipped (native host is already Linux-X64)"
fi

# --- 4. a bad digest must fail closed ---------------------------------------
# Asserts the security property, not just the exit code: on a mismatch nothing
# is exported AND no binary is extracted anywhere. That is what proves the
# verify-before-extract ordering actually holds.
echo "4. tampered digest fails closed"
t4="$(mktemp -d)"
zeros="$(printf '%064d' 0)"
sed -E "s/\"[0-9a-f]{64}\"/\"${zeros}\"/" "${here}/install.sh" > "${t4}/tampered.sh"
env RUNNER_OS="${native_os}" RUNNER_ARCH="${native_arch}" RUNNER_TEMP="${t4}" \
    GITHUB_PATH="${t4}/gh_path" GITHUB_ENV="${t4}/gh_env" \
    bash "${t4}/tampered.sh" > "${t4}/out.log" 2>&1
rc=$?
[ "${rc}" -ne 0 ] && pass "exited non-zero (${rc})" || fail "exited 0 on a bad digest"
grep -q "checksum mismatch" "${t4}/out.log" \
  && pass "reported a checksum mismatch" || fail "no checksum-mismatch error"
[ ! -s "${t4}/gh_env" ] \
  && pass "exported nothing to GITHUB_ENV" || fail "exported to GITHUB_ENV despite mismatch"
[ ! -s "${t4}/gh_path" ] \
  && pass "exported nothing to GITHUB_PATH" || fail "exported to GITHUB_PATH despite mismatch"
[ ! -e "${t4}/protoc-${version}/bin" ] \
  && pass "extracted nothing" || fail "an unverified archive was extracted"

# --- 5. an unsupported platform fails loudly --------------------------------
echo "5. unsupported platform fails loudly"
t5="$(mktemp -d)"
env RUNNER_OS=Plan9 RUNNER_ARCH=X64 RUNNER_TEMP="${t5}" \
    GITHUB_PATH="${t5}/gh_path" GITHUB_ENV="${t5}/gh_env" \
    bash "${here}/install.sh" > "${t5}/out.log" 2>&1
rc=$?
[ "${rc}" -ne 0 ] && pass "exited non-zero (${rc})" || fail "exited 0 on an unknown platform"
grep -q "install.sh" "${t5}/out.log" \
  && pass "error names the file to edit" || fail "error does not say where to add support"

echo
if [ "${failures}" -eq 0 ]; then
  echo "selftest: PASS"
else
  echo "selftest: FAIL (${failures})"
  exit 1
fi
```

- [ ] **Step 2: Run it to make sure it fails**

Run: `bash .github/actions/setup-protoc/selftest.sh`

Expected: FAIL. `install.sh` does not exist yet, so `awk` on it yields an empty `version`, every download 404s, and cases 2–5 fail. This confirms the harness actually exercises the file rather than passing vacuously.

- [ ] **Step 3: Write `install.sh`**

Create `.github/actions/setup-protoc/install.sh`:

```bash
#!/usr/bin/env bash
# Install a pinned, checksum-verified protoc and expose it to later steps.
#
# Runner contract — all read from the environment, all fakeable locally, which
# is how selftest.sh exercises this file:
#   RUNNER_OS, RUNNER_ARCH  select the release asset
#   RUNNER_TEMP             scratch space for the download
#   GITHUB_PATH             file; each line appended is prepended to PATH
#   GITHUB_ENV              file; each KEY=VALUE appended is exported
#
# Order is load-bearing: download -> verify -> extract -> export. An unverified
# archive must never reach an executable location.
set -euo pipefail

PROTOC_VERSION="35.1"

# Hand-bumped; nothing tracks these automatically. Dependabot follows action
# SHAs, and after SMA-458 there is no third-party action here for it to follow.
# Derived 2026-08-13 with `shasum -a 256` from:
#   https://github.com/protocolbuffers/protobuf/releases/tag/v35.1
# Bump runbook: CLAUDE.md, "protoc pin". Re-verify with selftest.sh.
SHA256_LINUX_X64="6930ebf62bd4ea607b98fff052596c6ee564b9835b4ce172c75a3f53ae9d91b7"
SHA256_MACOS_ARM64="193289af0470c6a1aada357d4fba0bbf8d78bfaac8b5e42ca30af2ef75583de2"
SHA256_WINDOWS_X64="5d3ff218d7d91eea95f7569bcb5a98f3030f8996d44151279d9772edcff76082"

case "${RUNNER_OS}-${RUNNER_ARCH}" in
  Linux-X64)   asset="linux-x86_64"; expected="${SHA256_LINUX_X64}";   exe="protoc"     ;;
  macOS-ARM64) asset="osx-aarch_64"; expected="${SHA256_MACOS_ARM64}"; exe="protoc"     ;;
  Windows-X64) asset="win64";        expected="${SHA256_WINDOWS_X64}"; exe="protoc.exe" ;;
  *)
    echo "::error::setup-protoc has no asset for ${RUNNER_OS}-${RUNNER_ARCH}. Add the asset name and its SHA-256 to .github/actions/setup-protoc/install.sh."
    exit 1
    ;;
esac

# RUNNER_TEMP is a Win32 path on Windows runners; bash needs the POSIX form.
if [ "${RUNNER_OS}" = "Windows" ]; then
  temp="$(cygpath -u "${RUNNER_TEMP}")"
else
  temp="${RUNNER_TEMP}"
fi

root="${temp}/protoc-${PROTOC_VERSION}"
archive="${temp}/protoc-${PROTOC_VERSION}-${asset}.zip"
rm -rf "${root}"
mkdir -p "${root}"

# The tag carries a leading v (v35.1); the filename does not (protoc-35.1-...).
# Mixing them 404s.
url="https://github.com/protocolbuffers/protobuf/releases/download/v${PROTOC_VERSION}/protoc-${PROTOC_VERSION}-${asset}.zip"

echo "setup-protoc: asset    ${asset}"
echo "setup-protoc: url      ${url}"

# Retries are not decoration: this runs in 11 job executions per PR, nine of
# them behind required contexts, plus the crates.io publish path.
curl -sSfL --retry 3 --retry-all-errors --retry-delay 2 \
     --connect-timeout 15 --max-time 300 \
     -o "${archive}" "${url}"

# macOS has no sha256sum; Linux runners and Git Bash do. Both accept the same
# "DIGEST  PATH" output format.
if command -v sha256sum > /dev/null 2>&1; then
  actual="$(sha256sum "${archive}" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "${archive}" | awk '{print $1}')"
fi

echo "setup-protoc: expected ${expected}"
echo "setup-protoc: actual   ${actual}"

if [ "${actual}" != "${expected}" ]; then
  echo "::error::protoc checksum mismatch for ${asset}. Likely causes, in order: (1) truncated or corrupted download; (2) the upstream release was re-tagged or its asset replaced; (3) tampering. DO NOT update the digest in install.sh without independently verifying the upstream release."
  exit 1
fi

# unzip preserves the archive's Unix mode. Expand-Archive and python zipfile do
# not, and the resulting non-executable protoc fails with "Permission denied"
# deep inside a cargo build script, far from here. 7z ships on the Windows
# runner image; unzip is not guaranteed in Git Bash.
if [ "${RUNNER_OS}" = "Windows" ]; then
  7z x -y -o"${root}" "${archive}" > /dev/null
else
  unzip -q "${archive}" -d "${root}"
  chmod +x "${root}/bin/${exe}"
fi

bin_dir="${root}/bin"
protoc_bin="${bin_dir}/${exe}"
include_dir="${root}/include"

# GITHUB_PATH and GITHUB_ENV are consumed by the runner, not by bash. On Windows
# a POSIX /d/a/... entry is a dead PATH entry to CreateProcess when cargo spawns
# protoc: it looks correct from inside bash and fails everywhere else.
if [ "${RUNNER_OS}" = "Windows" ]; then
  bin_dir="$(cygpath -w "${bin_dir}")"
  protoc_bin="$(cygpath -w "${protoc_bin}")"
  include_dir="$(cygpath -w "${include_dir}")"
fi

echo "${bin_dir}" >> "${GITHUB_PATH}"

# prost-build resolves PROTOC from the environment before falling back to a PATH
# lookup, so exporting it makes this install authoritative regardless of PATH
# ordering. PROTOC_INCLUDE carries the well-known types (google/protobuf/*.proto)
# that protoc would otherwise resolve relative to its own binary.
{
  echo "PROTOC=${protoc_bin}"
  echo "PROTOC_INCLUDE=${include_dir}"
} >> "${GITHUB_ENV}"

echo "setup-protoc: installed ${protoc_bin}"
```

- [ ] **Step 4: Write `verify.sh`**

Create `.github/actions/setup-protoc/verify.sh`:

```bash
#!/usr/bin/env bash
# Assert that the protoc later steps will actually use is the pinned one.
#
# This MUST run as its own composite step. GITHUB_PATH and GITHUB_ENV writes do
# not affect the step that makes them, only later steps — so an assertion living
# inside install.sh would validate a local `export PATH=`, not the mechanism
# cargo sees, and would be structurally blind to the propagation failure it
# exists to catch.
set -euo pipefail

EXPECTED_VERSION="libprotoc 35.1"

if [ -z "${PROTOC:-}" ]; then
  echo "::error::PROTOC is unset — install.sh did not export it, or GITHUB_ENV did not propagate."
  exit 1
fi

# PROTOC is a Win32 path on Windows; bash needs the POSIX form to exec it.
if [ "${RUNNER_OS:-}" = "Windows" ]; then
  protoc_bin="$(cygpath -u "${PROTOC}")"
  include_dir="$(cygpath -u "${PROTOC_INCLUDE}")"
else
  protoc_bin="${PROTOC}"
  include_dir="${PROTOC_INCLUDE}"
fi

# Windows protoc emits a trailing CR; a bare compare fails on a CORRECT install.
exported_version="$("${protoc_bin}" --version | tr -d '\r')"
resolved="$(command -v protoc || true)"

echo "setup-protoc: PROTOC   ${PROTOC}"
echo "setup-protoc: resolved ${resolved}"
echo "setup-protoc: version  ${exported_version}"

# 1. The compiler prost-build will actually use is the pinned one.
if [ "${exported_version}" != "${EXPECTED_VERSION}" ]; then
  echo "::error::PROTOC reports '${exported_version}', expected '${EXPECTED_VERSION}'."
  exit 1
fi

# 2. The PATH fallback resolves, and resolves to the same version. A version
#    check alone would pass if some other 35.1 protoc were first on PATH, so
#    the resolved location is logged for exactly that case.
if [ -z "${resolved}" ]; then
  echo "::error::protoc is not on PATH — GITHUB_PATH did not propagate."
  exit 1
fi

path_version="$(protoc --version | tr -d '\r')"
if [ "${path_version}" != "${EXPECTED_VERSION}" ]; then
  echo "::error::PATH resolves protoc to '${path_version}' at ${resolved}, expected '${EXPECTED_VERSION}'."
  exit 1
fi

# 3. The well-known types survived extraction. Without this sibling tree every
#    google/protobuf/*.proto import fails — which is every temporal proto.
if [ ! -f "${include_dir}/google/protobuf/timestamp.proto" ]; then
  echo "::error::well-known types missing at ${include_dir}/google/protobuf/ — extraction dropped the include tree."
  exit 1
fi

echo "setup-protoc: ok"
```

- [ ] **Step 5: Write `action.yml`**

Create `.github/actions/setup-protoc/action.yml`:

```yaml
name: "Setup protoc"
description: >-
  Install a pinned, checksum-verified protoc and expose it to later steps via
  PROTOC, PROTOC_INCLUDE and PATH. Replaces arduino/setup-protoc (SMA-458).

runs:
  using: composite
  steps:
    # Composite actions have NO default shell: `shell:` is mandatory on every
    # run step or the runner rejects the action outright.
    - name: Download and verify protoc
      shell: bash
      run: bash "${GITHUB_ACTION_PATH}/install.sh"

    # Deliberately a separate step — see the header comment in verify.sh.
    - name: Assert the pinned protoc is in effect
      shell: bash
      run: bash "${GITHUB_ACTION_PATH}/verify.sh"
```

- [ ] **Step 6: Run the self-test and make sure it passes**

Run: `bash .github/actions/setup-protoc/selftest.sh`

Expected: `selftest: PASS`, with every case reporting `ok`.

If case 2's `verify.sh` fails on `PROTOC is unset`, the likely cause is `install.sh` exiting before the export — read `install.log` in the reported temp dir.

- [ ] **Step 7: Lint the shell**

Run: `shellcheck .github/actions/setup-protoc/install.sh .github/actions/setup-protoc/verify.sh .github/actions/setup-protoc/selftest.sh`

Expected: clean. `actionlint` does **not** lint `action.yml`, so this is the only lint coverage the new logic gets. If `shellcheck` is not installed: `brew install shellcheck`.

- [ ] **Step 8: Commit**

```bash
git add .github/actions/setup-protoc/action.yml \
        .github/actions/setup-protoc/install.sh \
        .github/actions/setup-protoc/verify.sh \
        .github/actions/setup-protoc/selftest.sh
git commit -m "ci(workflows): SMA-458 add a checksum-verified setup-protoc action"
```

---

### Task 2: Migrate the 9 call sites and both path filters

**Files:**
- Modify: `.github/workflows/ci.yml` — 6 protoc sites + the `sessions-it` paths filter
- Modify: `.github/workflows/msrv.yml` — 1 protoc site
- Modify: `.github/workflows/release-plz.yml` — 1 protoc site
- Modify: `.github/workflows/integration.yml` — 1 protoc site + the `temporal-it` paths filter

**Interfaces:**
- Consumes: `./.github/actions/setup-protoc` from Task 1.
- Produces: no interface for later tasks; Task 3 documents what this establishes.

- [ ] **Step 1: Replace the 7 unguarded sites**

Six in `ci.yml` (jobs `clippy`, `test`, `build-no-default-features`, `docs`, `doc-coverage`), one in `msrv.yml`, one in `release-plz.yml`. Each currently looks like this — the comment wording varies slightly per site, the `uses:`/`with:` block does not:

```yaml
      # arduino/setup-protoc v3.0.0 — temporalio-protos compiles .proto at
      # build time (prost-build); a system protoc is required (SMA-332).
      - uses: arduino/setup-protoc@c65c819552d16ad3c9b72d9dfd5ba5237b9c906b
        with:
          repo-token: ${{ secrets.GITHUB_TOKEN }}
```

Replace each with:

```yaml
      # Pinned, checksum-verified protoc — temporalio-protos compiles .proto at
      # build time (prost-build); a system protoc is required (SMA-332). The
      # version and its digests live in the action (SMA-458).
      - uses: ./.github/actions/setup-protoc
```

Note the `with:` block goes away entirely: `repo-token` existed because the third-party action queried the GitHub *releases API*. This action builds the asset URL from a hardcoded version and makes no API call.

Keep each site's job-specific comment nuance where it exists — `ci.yml`'s `test` job notes cross-platform behaviour, and `release-plz.yml` explains that `cargo publish --verify` compiles the workspace. Preserve those sentences; only the mechanism sentence changes.

- [ ] **Step 2: Replace the 2 guarded sites, preserving their `if:`**

`ci.yml`, `sessions-it` job — the `if:` must survive:

```yaml
      # Pinned, checksum-verified protoc — temporalio-protos compiles .proto at
      # build time (prost-build); a system protoc is required (SMA-332).
      - if: steps.filter.outputs.sessions == 'true'
        uses: ./.github/actions/setup-protoc
```

`integration.yml`, `temporal-it` job:

```yaml
      # Pinned, checksum-verified protoc — temporalio-protos compiles .proto at
      # build time (prost-build); a system protoc is required (SMA-332).
      - if: steps.decide.outputs.run == 'true'
        uses: ./.github/actions/setup-protoc
```

- [ ] **Step 3: Confirm no `arduino/setup-protoc` reference survives**

Run: `grep -rn "arduino/setup-protoc" .github/`

Expected: no output. A surviving reference means a site was missed.

Then: `grep -rc "setup-protoc" .github/workflows/ci.yml`

Expected: `6`.

- [ ] **Step 4: Fix both paths filters**

This step prevents a regression *this change would otherwise introduce*. Both jobs currently filter on their own workflow file, which is where the protoc install used to live. After Task 2 it lives in `.github/actions/`, so without this the next protoc bump skips both jobs — including `sessions-it`, a **required** check, which would report green having run nothing protoc-related.

In `.github/workflows/ci.yml`, the `sessions-it` filter:

```yaml
          filters: |
            sessions:
              - 'Cargo.toml'
              - 'crates/paigasus-helikon-sessions-**'
              - 'crates/paigasus-helikon-core/src/session.rs'
              - '.github/workflows/ci.yml'
              - '.github/actions/**'
              - 'Cargo.lock'
```

In `.github/workflows/integration.yml`, the `temporal-it` filter:

```yaml
          filters: |
            temporal:
              - 'crates/paigasus-helikon-runtime-temporal/**'
              - 'crates/paigasus-helikon-core/src/**'
              - 'crates/paigasus-helikon-core/Cargo.toml'
              - 'Cargo.toml'
              - 'Cargo.lock'
              - '.github/workflows/integration.yml'
              - '.github/actions/**'
```

- [ ] **Step 5: Verify the workflows still parse and lint**

Run:

```bash
actionlint .github/workflows/*.yml
```

Expected: exactly one finding — SC2034, unused `i`, in `ci.yml`'s `sessions-it` readiness loop. Any second finding is a regression from this task.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/ci.yml .github/workflows/msrv.yml \
        .github/workflows/release-plz.yml .github/workflows/integration.yml
git commit -m "ci(workflows): SMA-458 use the local setup-protoc action at all 9 sites"
```

---

### Task 3: Documentation

**Files:**
- Modify: `CLAUDE.md` — CI section
- Modify: `CONTRIBUTING.md:148-158` — Build prerequisites

**Interfaces:**
- Consumes: the action and call sites from Tasks 1–2.
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Add the CI-section paragraph to CLAUDE.md**

Place it after the `integration.yml` paragraphs, before the `pr-title.yml` paragraph. Content to convey — write it in the file's existing voice (dense, reason-carrying prose, no bullet lists):

- `.github/actions/setup-protoc` is a **repo-local composite action** that installs protoc **35.1**, pinned exactly and verified against a per-platform SHA-256 before extraction. It replaced `arduino/setup-protoc` in SMA-458 at all 9 sites.
- The third-party action's `version` input **defaults to `23.x`, not to latest** — its README says otherwise and is wrong. CI had therefore been running **23.4**, so SMA-458 was also a deliberate 12-major upgrade, not the no-op its one-line framing implied.
- **Nothing tracks the pin.** Dependabot follows action SHAs, and there is no third-party action here any more. Bumping is a human act with no prompt — the same standing as `TEMPORAL_CLI_VERSION`/`TEMPORAL_CLI_SHA256` in `integration.yml` and `NIGHTLY_TOOLCHAIN` in `ci.yml`. **Bump runbook:** edit `PROTOC_VERSION` and all three digests in `install.sh` and `EXPECTED_VERSION` in `verify.sh`, then run `bash .github/actions/setup-protoc/selftest.sh`, which re-downloads each published asset and fails if any pinned digest disagrees.
- **Only three platforms are supported** (`Linux-X64`, `macOS-ARM64`, `Windows-X64`); anything else exits non-zero naming the file to edit. `linux-aarch_64` is deliberately absent even though `agentcore-image` runs on `ubuntu-24.04-arm` — an unexercised digest is an unexercised code path, and a wrong one would read as tampering rather than as a typo.
- **`verify.sh` must stay a separate step.** `$GITHUB_PATH`/`$GITHUB_ENV` writes do not affect the step that makes them, so an assertion folded back into `install.sh` would be blind to the propagation failure it exists to catch. It asserts `PROTOC`, the PATH fallback, and the presence of the well-known-type tree.
- **A checksum mismatch is not a signal to update the digest.** Causes are, in order: truncated download, upstream re-tag, tampering. Verify upstream independently first.
- **Availability trade, accepted knowingly:** a pinned URL does not self-heal. If protobuf removes or replaces the v35.1 assets, every required job and `release-plz` go red until a human bumps the pin.

- [ ] **Step 2: Fix the two stale claims in CONTRIBUTING.md**

`CONTRIBUTING.md:158` currently reads:

> If `protoc` is installed somewhere non-standard, point the `PROTOC` environment variable at the binary. CI installs it via `arduino/setup-protoc` in every job that compiles the workspace.

That second sentence becomes **factually false** on merge. Replace the sentence, and add the version, so contributors can match CI:

> If `protoc` is installed somewhere non-standard, point the `PROTOC` environment variable at the binary. CI installs **protoc 35.1** in every job that compiles the workspace, via the repo-local `.github/actions/setup-protoc` action, which verifies the download against a pinned SHA-256. Distribution packages often lag several majors behind — to match CI exactly, take the matching archive from the [protobuf v35.1 release](https://github.com/protocolbuffers/protobuf/releases/tag/v35.1) rather than relying on `apt`/`choco`.

Leave the `brew` / `apt-get` / `choco` snippet above it in place — it is still the easy path for a contributor who does not need an exact match.

- [ ] **Step 3: Check the docs gates**

Run:

```bash
mdbook build docs/book
```

Expected: clean. (No `docs/book/` page should need editing — this is a pure-internal CI change, a conscious skip under the CLAUDE.md currency rule, not an oversight. The build is run to confirm nothing was broken incidentally.)

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md CONTRIBUTING.md
git commit -m "docs(contributing): SMA-458 document the pinned protoc and its bump runbook"
```

---

## Pre-merge checklist

Not a task — the human-judgement gate before requesting merge.

- [ ] **`test (windows-latest, stable)` is green — confirmed by eye.** It is **NOT** a required context (`.github/rulesets/main-protection-checks.json` lists only `test (ubuntu-latest, stable)` and `test (macos-latest, stable)`), yet it is the *only* thing exercising `install.sh`'s Windows branch — `cygpath`, `7z`, `.exe`, and the `\r` strip. A red Windows job will not block the merge button. Check it manually.
- [ ] `test (macos-latest, stable)` green — the macOS branch, and a required context.
- [ ] **At least one digest independently recomputed by the reviewer.** The spec requires this: a digest nobody re-derived is a number, not a control. `selftest.sh` case 1 does all three automatically — confirm it ran and passed, or recompute by hand with `curl -sSL <asset-url> | shasum -a 256`.
- [ ] All other required contexts green: `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `book-build`, `commits`, `pr-title`, `audit`, `deny`, `sessions-it`, `build-no-default-features`.
- [ ] `sessions-it` actually **ran** rather than skipping — this PR touches `.github/workflows/ci.yml`, so its filter matches. Confirm in the log, since a skipped required job still reports green.
- [ ] **If CI is red, bisect mechanism vs compiler before debugging.** This PR combines a new install mechanism with a 23.4 → 35.1 upgrade. Set `PROTOC_VERSION="23.4"` in `install.sh`, `EXPECTED_VERSION="libprotoc 23.4"` in `verify.sh`, and swap the three digests for the 23.4 values recorded in the spec. Green on 23.4 and red on 35.1 isolates the compiler; red on both isolates the mechanism.
- [ ] **`release-plz.yml` is never exercised by this PR** — it runs only on `push` to `main`, so its first execution is post-merge against a live crates.io token. Watch that run. On failure, re-run: release-plz skips already-published versions, so a partial release completes rather than corrupts.

## Notes for the implementer

- **Work synchronously.** Do not background `cargo`, `actionlint`, or the self-test and end your turn — run them in the foreground and wait for a terminal result.
- **The self-test needs network access** and downloads roughly 20 MB across its cases.
- **Do not "fix" the SC2034 actionlint finding** in `ci.yml`'s `sessions-it` loop. It is pre-existing, unrelated, and out of scope.
