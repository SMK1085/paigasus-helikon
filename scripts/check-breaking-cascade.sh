#!/usr/bin/env bash
# check-breaking-cascade.sh — fail a pull request that gives a crate a breaking
# version bump, but gives a dependent crate only a non-breaking bump (SMA-838).
#
# Why: release-plz gives a dependent crate only a patch bump when a crate that
# it depends on gets a breaking bump. A user who upgrades the dependent crate
# then gets two incompatible versions of the dependency. This guard finds that
# case before the merge.
#
# The guard compares the version of each publishable workspace crate at the
# base revision with its version at the head (the working tree).
#
# Breaking bump:
#   0.0.z  any change of the version.
#   0.y.z  (y >= 1) y increases, or the major version increases.
#   x.y.z  (x >= 1) x increases.
# A version that goes down is an error.
#
# Rule: when a publishable crate has a breaking bump, each publishable crate
# with a normal dependency on it must also have a breaking bump. Each edge is
# checked. The rule is not transitive by itself.
#
# Exemption: paigasus-helikon-cli. It is a binary crate. Its library is
# internal, so a breaking bump in a dependency does not break a user of it.
#
# New crate: a crate is new only when no package with its name exists at the
# base. The guard matches by name, never by path. A new crate is skipped.
#
# Exit codes: 0 pass, 1 violation or loud failure. Messages go to stderr.
#
# Environment inputs (in CI they are all unset):
#   CASCADE_EVENT          overrides GITHUB_EVENT_NAME.
#   CASCADE_METADATA       path to `cargo metadata --no-deps --format-version 1`
#                          output for the head. Default: run that command.
#   CASCADE_BASE_VERSIONS  path to a file of "<name> <version>" lines for the
#                          base. Default: read each Cargo.toml at the base.
#   CASCADE_BASE_REV       overrides the base revision.
# Base revision: pull_request uses HEAD^1. A push event prints a notice and
# exits 0. An empty event (local run) uses the merge-base with origin/main.
# Any other event fails.
#
# Run locally: bash scripts/check-breaking-cascade.sh

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exempt="paigasus-helikon-cli"

die() {
  echo "error: $*" >&2
  exit 1
}

# --- event ------------------------------------------------------------------
# The event check comes before every other input.
event="${CASCADE_EVENT-${GITHUB_EVENT_NAME-}}"
case "${event}" in
  push)
    echo "notice: push event, no PR base; skipping"
    exit 0
    ;;
  pull_request | "") ;;
  *) die "unsupported event '${event}'; the guard runs on pull_request only" ;;
esac

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

# --- head packages ----------------------------------------------------------
if [[ -n "${CASCADE_METADATA-}" ]]; then
  [[ -f "${CASCADE_METADATA}" ]] || die "metadata file '${CASCADE_METADATA}' not found"
  metadata_file="${CASCADE_METADATA}"
else
  metadata_file="${tmp}/metadata.json"
  (cd "${repo_root}" && cargo metadata --no-deps --format-version 1) > "${metadata_file}" \
    || die "cargo metadata failed"
fi

# "<name> <version>" for each publishable member. publish is null (any
# registry) or a non-empty list; [] and false mean "do not publish".
jq -r '.packages[]
  | select(.publish != [] and .publish != false)
  | "\(.name) \(.version)"' "${metadata_file}" > "${tmp}/head.txt" \
  || die "cannot read the metadata file"
[[ -s "${tmp}/head.txt" ]] || die "no publishable workspace crate found"

# "<dependent> <dependency>" for each normal dependency between publishable
# members. A dev or build dependency has a non-null kind.
jq -r '[.packages[] | select(.publish != [] and .publish != false) | .name] as $pub
  | .packages[]
  | select(.publish != [] and .publish != false)
  | .name as $n
  | .dependencies[]
  | select(.kind == null and (.name as $d | $pub | index($d)))
  | "\($n) \(.name)"' "${metadata_file}" > "${tmp}/edges.txt" \
  || die "cannot read the dependencies from the metadata file"

# --- base versions ----------------------------------------------------------
if [[ -n "${CASCADE_BASE_VERSIONS-}" ]]; then
  [[ -f "${CASCADE_BASE_VERSIONS}" ]] \
    || die "base versions file '${CASCADE_BASE_VERSIONS}' not found"
  cp "${CASCADE_BASE_VERSIONS}" "${tmp}/base.txt"
else
  base="${CASCADE_BASE_REV-}"
  if [[ -z "${base}" ]]; then
    if [[ "${event}" == "pull_request" ]]; then
      base="HEAD^1"
    else
      base="$(git -C "${repo_root}" merge-base origin/main HEAD)" \
        || die "cannot find the merge-base with origin/main (run: git fetch origin main)"
    fi
  fi
  git -C "${repo_root}" rev-parse --verify --quiet "${base}^{commit}" > /dev/null \
    || die "base revision '${base}' not found (the checkout needs fetch-depth: 2)"
  git -C "${repo_root}" ls-tree -r --name-only "${base}" > "${tmp}/tree.txt" \
    || die "cannot list the files at '${base}'"
  : > "${tmp}/base.txt"
  # Skip build output and test fixtures. The head set decides which names
  # count, so a base entry for another name is ignored later. Limit: a
  # publishable crate under such a path would read as new and be skipped.
  grep -E '(^|/)Cargo\.toml$' "${tmp}/tree.txt" \
    | grep -Ev '(^|/)(target|tests|fixtures)/' > "${tmp}/manifests.txt" || true
  cut -d' ' -f1 "${tmp}/head.txt" > "${tmp}/head_names.txt"
  while IFS= read -r path; do
    content="$(git -C "${repo_root}" show "${base}:${path}")" \
      || die "cannot read '${path}' at '${base}'"
    # Take name and version from the [package] table only. A manifest without
    # [package] (the virtual workspace root) gives no output. Otherwise the
    # output is "<name>|<version>". A part that is not a literal string
    # (for example version.workspace = true) is empty. Both quote styles work.
    parsed="$(printf '%s\n' "${content}" | awk '
      function val(line,   s, q, i) {
        s = line; sub(/^[^=]*=[[:space:]]*/, "", s); q = substr(s, 1, 1)
        if (q != "\"" && q != "\047") return ""
        s = substr(s, 2); i = index(s, q)
        if (i == 0) return ""
        return substr(s, 1, i - 1)
      }
      /^[[:space:]]*\[/ { in_pkg = ($0 ~ /^[[:space:]]*\[package\][[:space:]]*(#.*)?$/); if (in_pkg) seen = 1; next }
      in_pkg && /^[[:space:]]*name[[:space:]]*=/ { name = val($0) }
      in_pkg && /^[[:space:]]*version[[:space:]]*=/ { ver = val($0) }
      END { if (seen) print name "|" ver }')"
    [[ -n "${parsed}" ]] || continue
    pkg_name="${parsed%%|*}"
    pkg_ver="${parsed#*|}"
    if [[ -z "${pkg_name}" ]]; then
      case "${path}" in
        tests/* | */tests/* | fixtures/* | */fixtures/*) continue ;;
      esac
      die "cannot read the package name of '${path}' at '${base}'"
    fi
    if [[ -z "${pkg_ver}" ]]; then
      grep -qxF -- "${pkg_name}" "${tmp}/head_names.txt" || continue
      die "cannot read the version of '${pkg_name}' in '${path}' at '${base}' (it needs a literal version = \"x.y.z\")"
    fi
    echo "${pkg_name} ${pkg_ver}" >> "${tmp}/base.txt"
  done < "${tmp}/manifests.txt"
fi

# --- version helpers --------------------------------------------------------
# split_version <version>: set V_MAJ, V_MIN, V_PAT as numbers. A pre-release or
# build suffix is dropped. A non-numeric part is an error.
split_version() {
  local core="${1%%[-+]*}"
  local IFS=.
  # shellcheck disable=SC2086
  set -- ${core}
  [[ $# -eq 3 ]] || die "version '${core}' is not x.y.z"
  local p
  for p in "$1" "$2" "$3"; do
    [[ "${p}" =~ ^[0-9]+$ ]] || die "version part '${p}' is not a number"
  done
  V_MAJ=$((10#$1))
  V_MIN=$((10#$2))
  V_PAT=$((10#$3))
}

# classify <old> <new>: print same, down, breaking or compatible.
classify() {
  split_version "$1"
  local o_maj=${V_MAJ} o_min=${V_MIN} o_pat=${V_PAT}
  split_version "$2"
  local n_maj=${V_MAJ} n_min=${V_MIN} n_pat=${V_PAT}
  if [[ ${o_maj} -eq ${n_maj} && ${o_min} -eq ${n_min} && ${o_pat} -eq ${n_pat} ]]; then
    echo same
    return
  fi
  # Compare numerically, one component at a time.
  local down=0
  if [[ ${n_maj} -lt ${o_maj} ]]; then
    down=1
  elif [[ ${n_maj} -eq ${o_maj} ]]; then
    if [[ ${n_min} -lt ${o_min} ]]; then
      down=1
    elif [[ ${n_min} -eq ${o_min} && ${n_pat} -lt ${o_pat} ]]; then
      down=1
    fi
  fi
  if [[ ${down} -eq 1 ]]; then
    echo down
    return
  fi
  if [[ ${o_maj} -eq 0 && ${o_min} -eq 0 ]]; then
    echo breaking
  elif [[ ${o_maj} -eq 0 ]]; then
    if [[ ${n_maj} -gt 0 || ${n_min} -gt ${o_min} ]]; then echo breaking; else echo compatible; fi
  else
    if [[ ${n_maj} -gt ${o_maj} ]]; then echo breaking; else echo compatible; fi
  fi
}

# min_breaking <old>: print the lowest version that is a breaking bump of <old>.
min_breaking() {
  split_version "$1"
  if [[ ${V_MAJ} -eq 0 && ${V_MIN} -eq 0 ]]; then
    echo "0.0.$((V_PAT + 1))"
  elif [[ ${V_MAJ} -eq 0 ]]; then
    echo "0.$((V_MIN + 1)).0"
  else
    echo "$((V_MAJ + 1)).0.0"
  fi
}

# --- status of each head crate ---------------------------------------------
# status.txt: "<name> <old> <new> <class>"; a new crate is not listed.
: > "${tmp}/status.txt"
errors=0
while read -r name head_ver; do
  base_ver="$(awk -v n="${name}" '$1 == n { print $2; exit }' "${tmp}/base.txt")"
  if [[ -z "${base_ver}" ]]; then
    echo "notice: ${name} is new at the head; skipped" >&2
    continue
  fi
  class="$(classify "${base_ver}" "${head_ver}")"
  if [[ "${class}" == "down" ]]; then
    echo "error: the version of ${name} goes down: ${base_ver} -> ${head_ver}" >&2
    errors=$((errors + 1))
  fi
  echo "${name} ${base_ver} ${head_ver} ${class}" >> "${tmp}/status.txt"
done < "${tmp}/head.txt"

[[ -s "${tmp}/status.txt" || ${errors} -gt 0 ]] \
  || die "no crate at the base matches a crate at the head"

# --- rule -------------------------------------------------------------------
violations=0
while read -r name old new class; do
  [[ "${class}" == "breaking" ]] || continue
  while read -r dependent dependency; do
    [[ "${dependency}" == "${name}" ]] || continue
    [[ "${dependent}" != "${exempt}" ]] || continue
    line="$(awk -v n="${dependent}" '$1 == n { print; exit }' "${tmp}/status.txt")"
    [[ -n "${line}" ]] || continue # new crate
    set -- ${line}
    dep_old="$2" dep_new="$3" dep_class="$4"
    [[ "${dep_class}" != "breaking" && "${dep_class}" != "down" ]] || continue
    if [[ ${violations} -eq 0 ]]; then
      echo "error: a crate has a breaking bump, but a dependent crate has none." >&2
    fi
    echo "  ${dependent}: ${dep_old} -> ${dep_new}, needs at least $(min_breaking "${dep_old}")" \
      "(depends on ${name}: ${old} -> ${new})" >&2
    violations=$((violations + 1))
  done < "${tmp}/edges.txt"
done < "${tmp}/status.txt"

if [[ ${violations} -gt 0 ]]; then
  cat >&2 <<'MSG'

How to fix: open a PR with a `!` in the title (for example `feat(core)!: ...`).
In that PR, add an upgrade note under `## [Unreleased]` in the CHANGELOG of each
crate listed above. Then let release-plz regenerate the release PR.
MSG
fi

if [[ ${errors} -gt 0 || ${violations} -gt 0 ]]; then
  exit 1
fi
echo "check-breaking-cascade: ok"
