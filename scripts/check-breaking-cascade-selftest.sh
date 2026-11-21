#!/usr/bin/env bash
# check-breaking-cascade-selftest.sh — pin the contract of
# check-breaking-cascade.sh (SMA-838).
#
# The self-test runs the script with fixture metadata and fixture base
# versions. It needs no git, no cargo and no network.
#
# Run: bash scripts/check-breaking-cascade-selftest.sh

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="${repo_root}/scripts/check-breaking-cascade.sh"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

failures=0

# make_metadata <file> <core> <dep-a> <facade> <cli> <devonly> <unpub> [dep-a-path]
# Each version argument is the HEAD version of that package.
# - dep-a: normal dependency on core.
# - facade: normal dependency on core and dep-a.
# - paigasus-helikon-cli: normal dependency on core.
# - devonly: dev dependency on core.
# - unpub: publish = [], normal dependency on core.
make_metadata() {
  local file="$1" core="$2" depa="$3" facade="$4" cli="$5" devonly="$6" unpub="$7"
  local depa_path="${8:-/ws/crates/dep-a/Cargo.toml}"
  jq -n \
    --arg core "${core}" --arg depa "${depa}" --arg facade "${facade}" \
    --arg cli "${cli}" --arg devonly "${devonly}" --arg unpub "${unpub}" \
    --arg depa_path "${depa_path}" '
    def dep($n; $k): {name: $n, kind: $k};
    {packages: [
      {name: "core", version: $core, publish: null,
       manifest_path: "/ws/crates/core/Cargo.toml", dependencies: []},
      {name: "dep-a", version: $depa, publish: null,
       manifest_path: $depa_path, dependencies: [dep("core"; null)]},
      {name: "facade", version: $facade, publish: null,
       manifest_path: "/ws/crates/facade/Cargo.toml",
       dependencies: [dep("core"; null), dep("dep-a"; null)]},
      {name: "paigasus-helikon-cli", version: $cli, publish: null,
       manifest_path: "/ws/crates/cli/Cargo.toml",
       dependencies: [dep("core"; null)]},
      {name: "dev-only", version: $devonly, publish: null,
       manifest_path: "/ws/crates/dev-only/Cargo.toml",
       dependencies: [dep("core"; "dev")]},
      {name: "unpub", version: $unpub, publish: [],
       manifest_path: "/ws/crates/unpub/Cargo.toml",
       dependencies: [dep("core"; null)]}
    ]}' > "${file}"
}

# make_base <file> <core> <dep-a> <facade> <cli> <devonly> <unpub>
make_base() {
  local file="$1"
  printf 'core %s\ndep-a %s\nfacade %s\npaigasus-helikon-cli %s\ndev-only %s\nunpub %s\n' \
    "$2" "$3" "$4" "$5" "$6" "$7" > "${file}"
}

# run_case <name> <expected exit> <expected substring or ""> <env VAR=val...>
run_case() {
  local name="$1" want_rc="$2" want_text="$3"
  shift 3
  local out rc=0
  out="$(env "$@" bash "${script}" 2>&1)" || rc=$?
  local ok=1
  if [[ "${rc}" -ne "${want_rc}" ]]; then ok=0; fi
  if [[ -n "${want_text}" && "${out}" != *"${want_text}"* ]]; then ok=0; fi
  if [[ "${ok}" -eq 1 ]]; then
    echo "PASS ${name}"
  else
    echo "FAIL ${name}: exit ${rc} (want ${want_rc}), want text '${want_text}'"
    echo "${out}" | sed 's/^/    | /'
    failures=$((failures + 1))
  fi
}

# case_versions <name> <rc> <text> <base: 6 versions> <head: 6 versions>
# Base order and head order: core dep-a facade cli devonly unpub.
case_versions() {
  local name="$1" rc="$2" text="$3"
  shift 3
  local d="${tmp}/${name}"
  mkdir -p "${d}"
  make_base "${d}/base.txt" "$1" "$2" "$3" "$4" "$5" "$6"
  make_metadata "${d}/meta.json" "$7" "$8" "$9" "${10}" "${11}" "${12}"
  run_case "${name}" "${rc}" "${text}" \
    CASCADE_EVENT=pull_request CASCADE_METADATA="${d}/meta.json" \
    CASCADE_BASE_VERSIONS="${d}/base.txt"
}

# Base: core 0.7.0, dep-a 0.2.0, facade 0.7.0, cli 0.1.0, dev-only 0.1.0, unpub 0.1.0.
B=(0.7.0 0.2.0 0.7.0 0.1.0 0.1.0 0.1.0)

case_versions no_version_change 0 "" "${B[@]}" 0.7.0 0.2.0 0.7.0 0.1.0 0.1.0 0.1.0
case_versions core_patch_dependents_patch 0 "" "${B[@]}" 0.7.1 0.2.1 0.7.1 0.1.1 0.1.0 0.1.0
case_versions core_breaking_all_breaking 0 "" "${B[@]}" 0.8.0 0.3.0 0.8.0 0.1.0 0.1.0 0.1.0
case_versions core_breaking_dependent_patch 1 "dep-a" "${B[@]}" 0.8.0 0.2.1 0.8.0 0.1.0 0.1.0 0.1.0
# The failure output must also name the minimum version.
case_versions core_breaking_dependent_patch_min 1 "0.3.0" "${B[@]}" 0.8.0 0.2.1 0.8.0 0.1.0 0.1.0 0.1.0
case_versions cli_exempt 0 "" "${B[@]}" 0.8.0 0.3.0 0.8.0 0.1.1 0.1.0 0.1.0
case_versions sibling_breaking_facade_patch 1 "facade" "${B[@]}" 0.7.0 0.3.0 0.7.1 0.1.0 0.1.0 0.1.0

case_versions major_scheme 1 "dep-a" \
  0.9.0 1.2.0 1.0.0 0.1.0 0.1.0 0.1.0 1.0.0 1.3.0 1.0.0 0.1.0 0.1.0 0.1.0
case_versions major_scheme_min 1 "2.0.0" \
  0.9.0 1.2.0 1.0.0 0.1.0 0.1.0 0.1.0 1.0.0 1.3.0 1.0.0 0.1.0 0.1.0 0.1.0

case_versions version_down 1 "goes down" "${B[@]}" 0.7.0 0.1.9 0.7.0 0.1.0 0.1.0 0.1.0

# new_crate_skipped: dep-a is absent at the base. Core breaks, dep-a is new.
d="${tmp}/new_crate_skipped"; mkdir -p "${d}"
printf 'core 0.7.0\nfacade 0.7.0\npaigasus-helikon-cli 0.1.0\ndev-only 0.1.0\nunpub 0.1.0\n' > "${d}/base.txt"
make_metadata "${d}/meta.json" 0.8.0 0.1.0 0.8.0 0.1.0 0.1.0 0.1.0
run_case new_crate_skipped 0 "" CASCADE_EVENT=pull_request \
  CASCADE_METADATA="${d}/meta.json" CASCADE_BASE_VERSIONS="${d}/base.txt"

# moved_crate_checked: the base list has names only. dep-a sits at another
# path at the head. The match is by name, so dep-a is an existing crate.
d="${tmp}/moved_crate_checked"; mkdir -p "${d}"
printf 'core 0.7.0\ndep-a 0.2.0\nfacade 0.7.0\npaigasus-helikon-cli 0.1.0\ndev-only 0.1.0\nunpub 0.1.0\n' > "${d}/base.txt"
make_metadata "${d}/meta.json" 0.8.0 0.2.1 0.8.0 0.1.0 0.1.0 0.1.0 /ws/elsewhere/moved/Cargo.toml
run_case moved_crate_checked 1 "dep-a" CASCADE_EVENT=pull_request \
  CASCADE_METADATA="${d}/meta.json" CASCADE_BASE_VERSIONS="${d}/base.txt"

# unreadable_base: the base versions file does not exist.
d="${tmp}/unreadable_base"; mkdir -p "${d}"
make_metadata "${d}/meta.json" 0.7.0 0.2.0 0.7.0 0.1.0 0.1.0 0.1.0
run_case unreadable_base 1 "not found" CASCADE_EVENT=pull_request \
  CASCADE_METADATA="${d}/meta.json" CASCADE_BASE_VERSIONS="${d}/missing.txt"

# empty_package_list_fails: no publishable package.
d="${tmp}/empty_package_list_fails"; mkdir -p "${d}"
printf '{"packages":[{"name":"unpub","version":"0.1.0","publish":[],"manifest_path":"/ws/u/Cargo.toml","dependencies":[]}]}\n' > "${d}/meta.json"
printf 'unpub 0.1.0\n' > "${d}/base.txt"
run_case empty_package_list_fails 1 "no publishable workspace crate" CASCADE_EVENT=pull_request \
  CASCADE_METADATA="${d}/meta.json" CASCADE_BASE_VERSIONS="${d}/base.txt"

# The event check happens before any other input is read.
run_case push_event_skips 0 "notice" CASCADE_EVENT=push \
  CASCADE_METADATA="${tmp}/none.json" CASCADE_BASE_VERSIONS="${tmp}/none.txt"
run_case unknown_event_fails 1 "unsupported event" CASCADE_EVENT=schedule \
  CASCADE_METADATA="${tmp}/none.json" CASCADE_BASE_VERSIONS="${tmp}/none.txt"

# 0.0.z scheme: every change is breaking.
case_versions zero_zero_scheme 1 "dep-a" \
  0.0.3 0.2.0 0.7.0 0.1.0 0.1.0 0.1.0 0.0.4 0.2.1 0.7.0 0.1.0 0.1.0 0.1.0
case_versions zero_zero_scheme_min 1 "0.3.0" \
  0.0.3 0.2.0 0.7.0 0.1.0 0.1.0 0.1.0 0.0.4 0.2.1 0.7.0 0.1.0 0.1.0 0.1.0
case_versions zero_zero_to_minor 1 "dep-a" \
  0.0.3 0.2.0 0.7.0 0.1.0 0.1.0 0.1.0 0.1.3 0.2.1 0.7.0 0.1.0 0.1.0 0.1.0

# Dev-only and unpublished dependents are ignored.
case_versions dev_and_unpublished_ignored 0 "" "${B[@]}" 0.8.0 0.3.0 0.8.0 0.1.0 0.1.1 0.1.1

# no_base_match_fails: the base lists only names that are absent at the head.
d="${tmp}/no_base_match_fails"; mkdir -p "${d}"
printf 'other-a 0.1.0\nother-b 0.2.0\n' > "${d}/base.txt"
make_metadata "${d}/meta.json" 0.7.0 0.2.0 0.7.0 0.1.0 0.1.0 0.1.0
run_case no_base_match_fails 1 "no crate at the base matches" CASCADE_EVENT=pull_request \
  CASCADE_METADATA="${d}/meta.json" CASCADE_BASE_VERSIONS="${d}/base.txt"

# Cases with the default base reader. They use a small local git repository.
# make_repo <dir> <dep-a manifest body>: commit a core and a dep-a manifest.
make_repo() {
  local dir="$1" depa_body="$2"
  mkdir -p "${dir}/crates/core" "${dir}/crates/dep-a"
  printf '[package]\nname = "core"\nversion = "0.7.0"\n' > "${dir}/crates/core/Cargo.toml"
  printf '%s\n' "${depa_body}" > "${dir}/crates/dep-a/Cargo.toml"
  printf '[workspace]\nmembers = ["crates/*"]\n' > "${dir}/Cargo.toml"
  git -C "${dir}" init -q
  git -C "${dir}" add -A
  git -C "${dir}" -c user.name=x -c user.email=x@x -c commit.gpgsign=false commit -q -m base
}

# Head: core is 0.8.0 (breaking), dep-a is 0.2.1 (patch).
d="${tmp}/gitcases"; mkdir -p "${d}"
make_metadata "${d}/meta.json" 0.8.0 0.2.1 0.8.0 0.1.0 0.1.0 0.1.0

make_repo "${d}/ws" '[package]
name = "dep-a"
version.workspace = true'
rev="$(git -C "${d}/ws" rev-parse HEAD)"
run_case base_version_workspace_fails 1 "crates/dep-a/Cargo.toml" CASCADE_EVENT=pull_request \
  CASCADE_METADATA="${d}/meta.json" CASCADE_BASE_REV="${rev}" \
  GIT_DIR="${d}/ws/.git" GIT_WORK_TREE="${d}/ws"

make_repo "${d}/ws2" "[package]
name = 'dep-a'
version = '0.2.0'"
rev="$(git -C "${d}/ws2" rev-parse HEAD)"
run_case base_single_quotes_read 1 "dep-a: 0.2.0 -> 0.2.1" CASCADE_EVENT=pull_request \
  CASCADE_METADATA="${d}/meta.json" CASCADE_BASE_REV="${rev}" \
  GIT_DIR="${d}/ws2/.git" GIT_WORK_TREE="${d}/ws2"

if [[ "${failures}" -ne 0 ]]; then
  echo "check-breaking-cascade-selftest: ${failures} case(s) failed" >&2
  exit 1
fi
echo "check-breaking-cascade-selftest: all cases passed"
