#!/usr/bin/env bash
# check-cargo-profile-env-sync.sh — assert the cargo-visible environment is
# uniform across every workflow that uses Swatinem/rust-cache.
#
# SMA-618. rust-cache hashes every environment variable whose name begins with
# CARGO, CC, CFLAGS, CXX, CMAKE or RUST into the cache key, alongside the rustc
# version (src/config.ts at the pinned SHA). Two jobs meant to share a cache
# entry share it only while they declare byte-identical values for all of them.
#
# Drift fails nothing on its own: the keys simply diverge, an extra
# multi-gigabyte entry appears, the repository crosses GitHub's fixed and
# un-raisable 10 GB cache limit, LRU eviction resumes, and CI legs start running
# cold on a random rotation — the exact condition SMA-618 exists to remove,
# returning silently and with no red gate. This script is the red gate.
#
# Two assertions:
#
#   1. Every workflow containing a `Swatinem/rust-cache` step declares an
#      identical set of *workflow-level* `env:` entries whose names match the
#      hashed prefixes above.
#   2. No such workflow declares a matching variable at *job* level, and no
#      rust-cache step carries its own `env:`. Job-level env is in scope when
#      the cache step runs; step-level env on a *different* step is not, which
#      is why RUSTDOCFLAGS on ci.yml's `docs` job (set on the `cargo doc` step,
#      which runs after the cache step) is legitimate and is not flagged.
#
# Known limitation, accepted for the same reason check-advisory-ignore-sync.sh
# accepts its own: this is a line-oriented parser, not a YAML parser. It depends
# on the repository's existing workflow formatting — a top-level `env:` at
# column 0 with entries at two spaces, jobs at two spaces with `env:` at four,
# steps at six with `env:` at eight. Reformatting the workflows means updating
# this script with them. scripts/check-cargo-profile-env-sync-selftest.sh pins
# that contract; run it after any change here.
#
# Assertion 2 is deliberately conservative: it flags job-level matches in *any*
# job of a cache-bearing workflow, not only in jobs that themselves have a cache
# step. A false positive costs one comment; a false negative costs the budget.
#
# Escape hatch: a workflow that legitimately needs a divergent cargo-visible
# variable is listed in NON_SHARING below, with a comment saying why.
#
# Usage: bash scripts/check-cargo-profile-env-sync.sh [WORKFLOW_DIR]
#   exit 0 — agreement
#   exit 1 — drift (the failure this guards)
#   exit 2 — usage or environment error

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workflow_dir="${1:-${repo_root}/.github/workflows}"

# Workflows exempted from assertion 1. Empty today; add an entry with a reason
# rather than loosening the prefix list.
NON_SHARING=()

# The prefixes rust-cache hashes. Keep in step with src/config.ts at the pinned
# SHA; a prefix added upstream and missed here is a silent hole in the guard.
HASHED_PREFIX_RE='^(CARGO|CC|CFLAGS|CXX|CMAKE|RUST)'

if [[ ! -d "${workflow_dir}" ]]; then
  echo "error: workflow directory not found: ${workflow_dir}" >&2
  exit 2
fi

is_exempt() {
  local needle="$1" w
  for w in ${NON_SHARING[@]+"${NON_SHARING[@]}"}; do
    [[ "${w}" == "${needle}" ]] && return 0
  done
  return 1
}

# Emit the workflow-level env entries matching the hashed prefixes, sorted.
# The block runs from a column-0 `env:` to the next column-0 token.
workflow_env() {
  awk '
    /^env:[[:space:]]*$/ { inblock = 1; next }
    inblock && /^[^[:space:]#]/ { inblock = 0 }
    inblock && /^  [A-Za-z_][A-Za-z0-9_]*:/ { sub(/^  /, ""); print }
  ' "$1" | grep -E "${HASHED_PREFIX_RE}" | sort || true
}

# Emit job-level env entry NAMES matching the hashed prefixes (assertion 2a).
job_env_names() {
  awk '
    /^    env:[[:space:]]*$/ { inblock = 1; next }
    inblock && /^ {0,4}[^ #]/ { inblock = 0 }
    inblock && /^      [A-Za-z_][A-Za-z0-9_]*:/ {
      sub(/^      /, ""); sub(/:.*$/, ""); print
    }
  ' "$1" | grep -E "${HASHED_PREFIX_RE}" | sort -u || true
}

# Emit a marker line per rust-cache step that carries its own env: (2b).
cache_step_env() {
  awk '
    /Swatinem\/rust-cache@/ { instep = 1; next }
    instep && /^      - / { instep = 0 }
    instep && /^        env:[[:space:]]*$/ { print FILENAME ": rust-cache step has its own env:" }
  ' "$1" || true
}

mapfile -t all_workflows < <(find "${workflow_dir}" -maxdepth 1 -name '*.yml' | sort)
if [[ "${#all_workflows[@]}" == "0" ]]; then
  echo "error: no workflow files in ${workflow_dir}" >&2
  exit 2
fi

cached=()
for f in "${all_workflows[@]}"; do
  grep -q 'Swatinem/rust-cache@' "${f}" && cached+=("${f}")
done

if [[ "${#cached[@]}" == "0" ]]; then
  echo "error: no workflow uses Swatinem/rust-cache; this guard is misplaced" >&2
  exit 2
fi

problems=()

# --- assertion 1: identical workflow-level cargo-visible env ---------------
baseline=""
baseline_file=""
for f in "${cached[@]}"; do
  name="$(basename "${f}")"
  is_exempt "${name}" && continue
  env_block="$(workflow_env "${f}")"
  if [[ -z "${baseline_file}" ]]; then
    baseline="${env_block}"
    baseline_file="${name}"
    continue
  fi
  if [[ "${env_block}" != "${baseline}" ]]; then
    problems+=("${name}: workflow-level cargo-visible env differs from ${baseline_file}")
    problems+=("  ${baseline_file}: $(printf '%s' "${baseline}"  | tr '\n' ' ')")
    problems+=("  ${name}: $(printf '%s' "${env_block}" | tr '\n' ' ')")
  fi
done

# --- assertion 2: no job-level, and none on the cache step ----------------
for f in "${cached[@]}"; do
  name="$(basename "${f}")"
  while IFS= read -r var; do
    [[ -n "${var}" ]] || continue
    problems+=("${name}: job-level env '${var}' is inside the cache key; move it to the workflow-level env: block")
  done < <(job_env_names "${f}")
  while IFS= read -r hit; do
    [[ -n "${hit}" ]] || continue
    problems+=("${name}: a Swatinem/rust-cache step declares its own env:; it is hashed into that job's key alone")
  done < <(cache_step_env "${f}")
done

if [[ "${#problems[@]}" == "0" ]]; then
  echo "cargo-visible workflow env agrees across ${#cached[@]} cache-bearing workflow(s)"
  printf '%s' "${baseline}" | sed 's/^/  /'
  echo
  exit 0
fi

{
  echo "error: the cargo-visible environment has drifted between workflows"
  echo
  printf '  %s\n' "${problems[@]}"
  echo
  echo "These variable names are hashed into the rust-cache cache key, so drift"
  echo "silently splits a shared cache entry in two and pushes the repository"
  echo "back over GitHub's 10 GB limit. See docs/runbooks/ci-architecture.md"
  echo "-> Actions cache budget."
} >&2

exit 1
