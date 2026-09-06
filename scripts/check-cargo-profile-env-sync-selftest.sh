#!/usr/bin/env bash
# check-cargo-profile-env-sync-selftest.sh — pin the contract of
# check-cargo-profile-env-sync.sh.
#
# That script is a line-oriented parser, not a YAML parser (see its header for
# why). This self-test is what makes that acceptable: it asserts the parser's
# behaviour against synthetic fixtures covering each way the real workflows are
# shaped, so a reformatting that breaks the parser fails here rather than
# silently reporting agreement in a required job.
#
# Run: bash scripts/check-cargo-profile-env-sync-selftest.sh

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="${repo_root}/scripts/check-cargo-profile-env-sync.sh"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

failures=0

# Assert the script's exit status against a fixture directory.
#   expect <expected-status> <case-name> <fixture-dir>
expect() {
  local want="$1" name="$2" dir="$3" got=0
  bash "${script}" "${dir}" >"${tmp}/out.txt" 2>&1 || got=$?
  if [[ "${got}" == "${want}" ]]; then
    echo "ok    — ${name}"
  else
    echo "FAIL  — ${name}: expected exit ${want}, got ${got}" >&2
    sed 's/^/        /' "${tmp}/out.txt" >&2
    failures=$((failures + 1))
  fi
}

# A cache-bearing workflow with the given workflow-level env body and an
# optional extra block spliced into the job.
#   make_workflow <dir> <name> <env-body> [extra-job-yaml]
make_workflow() {
  local dir="$1" name="$2" env_body="$3" extra="${4:-}"
  mkdir -p "${dir}"
  {
    echo "name: ${name}"
    echo "on:"
    echo "  push:"
    echo "    branches: [main]"
    echo "env:"
    printf '%s\n' "${env_body}"
    echo ""
    echo "jobs:"
    echo "  build:"
    echo "    runs-on: ubuntu-latest"
    [[ -n "${extra}" ]] && printf '%s\n' "${extra}"
    echo "    steps:"
    echo "      # Swatinem/rust-cache v2.9.2"
    echo "      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6"
    echo "      - run: cargo build"
  } > "${dir}/${name}.yml"
}

# --- case 1: two cache-bearing workflows that agree ------------------------
d="${tmp}/agree"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
make_workflow "${d}" beta  "  CARGO_TERM_COLOR: always"
expect 0 "identical cargo-visible env passes" "${d}"

# --- case 2: one workflow missing the variable entirely -------------------
d="${tmp}/missing"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
make_workflow "${d}" beta  "  NIGHTLY_TOOLCHAIN: nightly-2026-05-01"
expect 1 "a workflow missing a cargo-visible var fails" "${d}"

# --- case 3: same names, different values ---------------------------------
d="${tmp}/values"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
make_workflow "${d}" beta  "  CARGO_TERM_COLOR: never"
expect 1 "a differing value fails" "${d}"

# --- case 4: non-matching prefixes are ignored ----------------------------
# NIGHTLY_TOOLCHAIN / HELIKON_* are not hashed by rust-cache, so they may
# differ freely. This guards against an over-broad prefix match.
d="${tmp}/ignored"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always
  NIGHTLY_TOOLCHAIN: nightly-2026-05-01"
make_workflow "${d}" beta  "  CARGO_TERM_COLOR: always"
expect 0 "non-hashed variables may differ" "${d}"

# --- case 5: a workflow with no rust-cache step is out of scope -----------
d="${tmp}/uncached"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
mkdir -p "${d}"
cat > "${d}/uncached.yml" <<'YAML'
name: uncached
on:
  push:
    branches: [main]
env:
  CARGO_TERM_COLOR: never
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo no cache here
YAML
expect 0 "a workflow without rust-cache is not compared" "${d}"

# --- case 6: job-level cargo-visible env is rejected ----------------------
d="${tmp}/joblevel"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
make_workflow "${d}" beta  "  CARGO_TERM_COLOR: always" "    env:
      CARGO_INCREMENTAL: 0"
expect 1 "job-level cargo-visible env fails" "${d}"

# --- case 7: job-level env with a non-hashed name is allowed --------------
# ci.yml really does this: `test` sets HELIKON_REQUIRE_SANDBOX and
# `doc-coverage` sets DOC_COVERAGE_THRESHOLD at job level.
d="${tmp}/joblevel-ok"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
make_workflow "${d}" beta  "  CARGO_TERM_COLOR: always" "    env:
      HELIKON_REQUIRE_SANDBOX: 1"
expect 0 "job-level non-hashed env is allowed" "${d}"

# --- case 8: env on the rust-cache step itself is rejected ----------------
d="${tmp}/stepenv"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
mkdir -p "${d}"
cat > "${d}/beta.yml" <<'YAML'
name: beta
on:
  push:
    branches: [main]
env:
  CARGO_TERM_COLOR: always

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      # Swatinem/rust-cache v2.9.2
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6
        env:
          CARGO_INCREMENTAL: 0
      - run: cargo build
YAML
expect 1 "env on the rust-cache step itself fails" "${d}"

# --- case 9: step-level env on a LATER step is allowed --------------------
# ci.yml's `docs` job sets RUSTDOCFLAGS on its `cargo doc` step. That step runs
# after the cache step, so the variable is absent when the key is computed.
d="${tmp}/laterstep"
make_workflow "${d}" alpha "  CARGO_TERM_COLOR: always"
mkdir -p "${d}"
cat > "${d}/beta.yml" <<'YAML'
name: beta
on:
  push:
    branches: [main]
env:
  CARGO_TERM_COLOR: always

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      # Swatinem/rust-cache v2.9.2
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6
      - run: cargo doc
        env:
          RUSTDOCFLAGS: "-D warnings"
YAML
expect 0 "step-level env on a later step is allowed" "${d}"

# --- case 10: the real workflows pass -------------------------------------
expect 0 "the repository's own workflows agree" "${repo_root}/.github/workflows"

echo
if [[ "${failures}" == "0" ]]; then
  echo "check-cargo-profile-env-sync selftest: all cases passed"
  exit 0
fi
echo "check-cargo-profile-env-sync selftest: ${failures} case(s) failed" >&2
exit 1
