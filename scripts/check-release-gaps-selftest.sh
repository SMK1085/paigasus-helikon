#!/usr/bin/env bash
# check-release-gaps-selftest.sh — pin the contract of check-release-gaps.sh
# (SMA-745).
#
# No pull request runs release-plz.yml. The gap report runs only on `main`, so
# this self-test is the only check of the report before a merge. It runs the
# script with fake `curl` and `gh` commands at the front of PATH and with
# fixture metadata. It makes no network call and runs no cargo command.
#
# Each fake answers from fixture files in a per-case directory (FAKE_DIR):
#   - fake curl: FAKE_DIR/crates/<name>.status and <name>.body. A missing
#     status file means "no response": the fake prints 000 and exits 28, as
#     curl does on a time-out.
#   - fake gh: FAKE_DIR/gh/<api path with "/" replaced by "__">. The file holds
#     the full `gh api --include` output. A missing file means no output.
# Each fake writes every call to FAKE_DIR/calls.log. A call for the crate
# `internal` (publish = []) writes FAKE_DIR/violations, and the case fails.
#
# Run: bash scripts/check-release-gaps-selftest.sh

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="${repo_root}/scripts/check-release-gaps.sh"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

failures=0

# --- fake commands ----------------------------------------------------------
mkdir -p "${tmp}/bin"

cat > "${tmp}/bin/curl" <<'FAKE'
#!/usr/bin/env bash
# Fake curl. It knows only the call form of check-release-gaps.sh.
set -u
out="" url=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    -A|-w|--max-time) shift 2 ;;
    -*) shift ;;
    *) url="$1"; shift ;;
  esac
done
echo "curl ${url}" >> "${FAKE_DIR}/calls.log"
name="${url##*/}"
if [[ "${url}" != "https://crates.io/api/v1/crates/${name}" ]]; then
  echo "unexpected curl URL: ${url}" >> "${FAKE_DIR}/violations"
fi
if [[ "${name}" == "internal" ]]; then
  echo "curl called for internal" >> "${FAKE_DIR}/violations"
fi
status_file="${FAKE_DIR}/crates/${name}.status"
if [[ ! -f "${status_file}" ]]; then
  printf '000'
  echo "curl: (28) Operation timed out" >&2
  exit 28
fi
if [[ -f "${FAKE_DIR}/crates/${name}.body" ]]; then
  cp "${FAKE_DIR}/crates/${name}.body" "${out}"
else
  : > "${out}"
fi
printf '%s' "$(cat "${status_file}")"
exit 0
FAKE

cat > "${tmp}/bin/gh" <<'FAKE'
#!/usr/bin/env bash
# Fake gh. It knows only `gh api --include <path>`.
set -u
echo "gh $*" >> "${FAKE_DIR}/calls.log"
if [[ $# -ne 3 || "$1" != "api" || "$2" != "--include" ]]; then
  echo "unexpected gh call: $*" >> "${FAKE_DIR}/violations"
  exit 2
fi
path="$3"
case "${path}" in
  *internal*) echo "gh called for internal" >> "${FAKE_DIR}/violations" ;;
esac
key="$(printf '%s' "${path}" | sed 's#/#__#g')"
file="${FAKE_DIR}/gh/${key}"
if [[ ! -f "${file}" ]]; then
  echo "fake gh: no fixture for ${path}" >&2
  exit 1
fi
cat "${file}"
# gh exits non-zero on an HTTP error status. The script must ignore this.
case "$(head -n 1 "${file}")" in
  "HTTP/"*" 2"*) exit 0 ;;
  *) exit 1 ;;
esac
FAKE

chmod +x "${tmp}/bin/curl" "${tmp}/bin/gh"

# --- fixture helpers --------------------------------------------------------

# Write metadata with the given packages. Each argument is "name version
# publish-json", for example: "alpha 1.0.0 null".
#   write_metadata <file> <package>...
write_metadata() {
  local file="$1" first=1 name ver pub
  shift
  {
    echo '{"workspace_root":"/ws","packages":['
    for spec in "$@"; do
      read -r name ver pub <<<"${spec}"
      [[ "${first}" == 1 ]] || echo ','
      first=0
      printf '{"name":"%s","version":"%s","publish":%s,"manifest_path":"/ws/crates/%s/Cargo.toml"}\n' \
        "${name}" "${ver}" "${pub}" "${name}"
    done
    echo ']}'
  } > "${file}"
}

# Make crates.io answer for a crate.
#   crate_fixture <name> <status> <body>
crate_fixture() {
  printf '%s' "$2" > "${FAKE_DIR}/crates/$1.status"
  printf '%s' "$3" > "${FAKE_DIR}/crates/$1.body"
}

# Make crates.io list the given version, after an older one.
#   crate_published <name> <version>
crate_published() {
  crate_fixture "$1" 200 \
    "{\"crate\":{\"name\":\"$1\",\"num_versions\":2},\"versions\":[{\"num\":\"$2\",\"yanked\":false},{\"num\":\"0.0.1\",\"yanked\":false}]}"
}

# Make gh answer for an API path. Lines end with CRLF, as HTTP headers do.
#   gh_fixture <path> <status-line> [body]
gh_fixture() {
  local key
  key="$(printf '%s' "$1" | sed 's#/#__#g')"
  {
    printf '%s\r\n' "$2"
    printf 'content-type: application/json; charset=utf-8\r\n'
    printf 'x-github-media-type: github.v3; format=json\r\n'
    printf '\r\n'
    printf '%s' "${3:-}"
  } > "${FAKE_DIR}/gh/${key}"
}

# Remove the gh answer for an API path.
#   gh_remove <path>
gh_remove() {
  rm -f "${FAKE_DIR}/gh/$(printf '%s' "$1" | sed 's#/#__#g')"
}

tag_ok() {
  gh_fixture "repos/o/r/git/ref/tags/$1" 'HTTP/2.0 200 OK' \
    "{\"ref\":\"refs/tags/$1\",\"object\":{\"type\":\"tag\",\"sha\":\"abc\"}}"
}

release_ok() {
  gh_fixture "repos/o/r/releases/tags/$1" 'HTTP/2 200' \
    "{\"tag_name\":\"$1\",\"draft\":false}"
}

# Start a new case with the base "all good" fixtures: alpha 1.0.0 and
# beta 2.0.0 are published, tagged and released. internal 0.0.0 has
# publish = [] and must not be checked.
#   new_case <dir-name>
new_case() {
  FAKE_DIR="${tmp}/case-$1"
  export FAKE_DIR
  mkdir -p "${FAKE_DIR}/crates" "${FAKE_DIR}/gh"
  : > "${FAKE_DIR}/calls.log"

  write_metadata "${FAKE_DIR}/metadata.json" \
    'alpha 1.0.0 null' 'beta 2.0.0 ["crates-io"]' 'internal 0.0.0 []'
  cat > "${FAKE_DIR}/release-plz.toml" <<'TOML'
[workspace]
dependencies_update = true
pr_name = "chore: release"
TOML

  crate_published alpha 1.0.0
  crate_published beta 2.0.0
  tag_ok alpha-v1.0.0
  tag_ok beta-v2.0.0
  release_ok alpha-v1.0.0
  release_ok beta-v2.0.0

  export RELEASE_GAPS_METADATA="${FAKE_DIR}/metadata.json"
  export RELEASE_GAPS_CONFIG="${FAKE_DIR}/release-plz.toml"
  export RELEASE_GAPS_SLEEP=0
  export GITHUB_REPOSITORY=o/r
  export GITHUB_SHA=deadbeef
  # CI runs this self-test in a job that sets these. Clear them, so that a
  # case never writes to the real job summary.
  unset RELEASE_STEP_OUTCOME GITHUB_STEP_SUMMARY
}

# Run the script and check its exit status and its output. A pattern that
# starts with "!" must NOT be in the output.
#   expect <expected-status> <case-name> [[!]<fixed-string-pattern>...]
expect() {
  local want="$1" name="$2" got=0 bad=""
  shift 2
  PATH="${tmp}/bin:${PATH}" bash "${script}" \
    >"${FAKE_DIR}/stdout.txt" 2>"${FAKE_DIR}/stderr.txt" || got=$?
  cat "${FAKE_DIR}/stdout.txt" "${FAKE_DIR}/stderr.txt" > "${FAKE_DIR}/all.txt"
  if [[ "${got}" != "${want}" ]]; then
    bad="expected exit ${want}, got ${got}"
  fi
  for pattern in "$@"; do
    if [[ "${pattern}" == '!'* ]]; then
      if grep -qF -- "${pattern#!}" "${FAKE_DIR}/all.txt"; then
        bad="${bad:+${bad}; }output contains '${pattern#!}'"
      fi
    elif ! grep -qF -- "${pattern}" "${FAKE_DIR}/all.txt"; then
      bad="${bad:+${bad}; }output does not contain '${pattern}'"
    fi
  done
  if [[ -s "${FAKE_DIR}/violations" ]]; then
    bad="${bad:+${bad}; }fake commands saw a bad call: $(tr '\n' ' ' < "${FAKE_DIR}/violations")"
  fi
  report "${name}" "${bad}"
}

# Check an extra condition on the last run.
#   check <case-name> <description> <command>...
check() {
  local name="$1" what="$2"
  shift 2
  if "$@"; then
    report "${name}: ${what}" ""
  else
    report "${name}: ${what}" "condition is false"
  fi
}

# Print one result line.
#   report <case-name> <problem-or-empty>
report() {
  if [[ -z "$2" ]]; then
    echo "ok    — $1"
  else
    echo "FAIL  — $1: $2" >&2
    sed 's/^/        /' "${FAKE_DIR}/all.txt" >&2
    failures=$((failures + 1))
  fi
}

# The helpers below run through `check`, so shellcheck does not see the calls.
# shellcheck disable=SC2329
no_call_for() {
  ! grep -q "$1" "${FAKE_DIR}/calls.log"
}

# shellcheck disable=SC2329
contains() {
  grep -qF -- "$2" "$1"
}

# shellcheck disable=SC2329
no_error_in() {
  [[ -f "$1" ]] && ! grep -q '::error::' "$1"
}

# shellcheck disable=SC2329
one_error_line() {
  [[ "$(grep -c '^::error::' "$1")" == 1 && "$(grep -c '::error::' "$1")" == 1 ]]
}

# --- cases --------------------------------------------------------------------

new_case all-good
expect 0 "all versions tagged and released" "no gaps"

new_case tag-missing
gh_fixture repos/o/r/git/ref/tags/alpha-v1.0.0 'HTTP/1.1 404 Not Found' '{"message":"Not Found"}'
# The backticks are literal text in the report.
# shellcheck disable=SC2016
expect 1 "published, no tag" "alpha" "1.0.0" \
  'tag `alpha-v1.0.0` is missing' "object=deadbeef" \
  "repos/o/r/git/tags/<tag-object-sha> --jq" "use the commit that bumped the version" \
  '!GitHub release `alpha-v1.0.0` is missing' "!gh release create"

new_case release-missing
gh_fixture repos/o/r/releases/tags/alpha-v1.0.0 'HTTP/2 404' '{"message":"Not Found"}'
# The backticks are literal text in the report.
# shellcheck disable=SC2016
expect 1 "published, tag present, no release" "alpha" \
  'GitHub release `alpha-v1.0.0` is missing' "gh release create alpha-v1.0.0" \
  "gh release edit alpha-v1.0.0 --draft=false --latest=false" \
  '!tag `alpha-v1.0.0` is missing' "!git/tags"

new_case release-draft
gh_fixture repos/o/r/releases/tags/alpha-v1.0.0 'HTTP/2 200' '{"tag_name":"alpha-v1.0.0","draft":true}'
expect 1 "published, release is a draft" "alpha" "draft"

new_case not-published
crate_published alpha 0.9.0
gh_remove repos/o/r/git/ref/tags/alpha-v1.0.0
gh_remove repos/o/r/releases/tags/alpha-v1.0.0
expect 0 "version not in the crates.io list"
check "version not in the crates.io list" "no tag or release call for alpha" \
  no_call_for "gh api --include repos/o/r/.*alpha"

new_case yanked
crate_fixture alpha 200 \
  '{"crate":{"name":"alpha","num_versions":2},"versions":[{"num":"1.0.0","yanked":true},{"num":"0.9.0","yanked":false}]}'
gh_fixture repos/o/r/git/ref/tags/alpha-v1.0.0 'HTTP/1.1 404 Not Found' '{"message":"Not Found"}'
expect 1 "a yanked version counts as published" "alpha"

new_case crates-404
crate_fixture alpha 404 '{"errors":[{"detail":"crate alpha does not exist"}]}'
expect 2 "crates.io returns 404 for the crate" "could not check" "alpha"

new_case crates-500
crate_fixture alpha 500 'Internal Server Error'
expect 2 "crates.io returns 500" "could not check"

new_case crates-429
crate_fixture alpha 429 '{"errors":[{"detail":"rate limited"}]}'
expect 2 "crates.io returns 429" "could not check"

new_case crates-no-response
rm -f "${FAKE_DIR}/crates/alpha.status" "${FAKE_DIR}/crates/alpha.body"
expect 2 "crates.io gives no response" "could not check"

new_case crates-bad-json
crate_fixture alpha 200 '<html>maintenance</html>'
expect 2 "crates.io returns 200 with a body that is not JSON" "could not check"

new_case crates-no-versions
crate_fixture alpha 200 '{"crate":{"name":"alpha"}}'
expect 2 "crates.io returns 200 without a versions list" "could not check"

new_case crates-truncated
# The list has two versions, but crates.io says the crate has three. The
# missing one can be the version under check.
crate_fixture alpha 200 \
  '{"crate":{"name":"alpha","num_versions":3},"versions":[{"num":"1.0.0","yanked":false},{"num":"0.9.0","yanked":false}]}'
expect 2 "crates.io versions list shorter than num_versions" "could not check" "num_versions is 3"

new_case crates-no-count
crate_fixture alpha 200 \
  '{"crate":{"name":"alpha"},"versions":[{"num":"1.0.0","yanked":false}]}'
expect 2 "crates.io gives no num_versions" "could not check" "num_versions is missing"

new_case crates-crate-not-object
# "crate" is a string, so the count query fails (jq exit 5). The crate must
# not pass the count check as "checked".
crate_fixture alpha 200 \
  '{"crate":"x","versions":[{"num":"1.0.0","yanked":false}]}'
expect 2 "crates.io gives a crate field that is not an object" "could not check" \
  "not readable"

new_case summary-unwritable
export GITHUB_STEP_SUMMARY="${FAKE_DIR}/no-such-dir/summary.md"
expect 0 "an unwritable job summary does not change the result" \
  "no gaps" "::warning::cannot write job summary"

new_case tag-500
gh_fixture repos/o/r/git/ref/tags/alpha-v1.0.0 'HTTP/2.0 500 Internal Server Error' '{}'
expect 2 "tag call returns 500" "could not check" "alpha"

new_case release-401
gh_fixture repos/o/r/releases/tags/alpha-v1.0.0 'HTTP/2 401' '{"message":"Bad credentials"}'
expect 2 "release call returns 401" "could not check" "alpha"

new_case gh-no-output
gh_remove repos/o/r/git/ref/tags/alpha-v1.0.0
expect 2 "tag call gives no status line" "could not check" "alpha"

new_case gap-and-unknown
gh_fixture repos/o/r/git/ref/tags/alpha-v1.0.0 'HTTP/1.1 404 Not Found' '{"message":"Not Found"}'
crate_fixture beta 500 'Internal Server Error'
expect 1 "one gap plus one could-not-check item" "alpha" "could not check" "beta"

new_case longer-tag
# Only alpha-v1.0.0X exists. The prefix-match endpoint (plural "refs") finds
# it, and the exact endpoint (singular "ref") does not.
gh_fixture repos/o/r/git/ref/tags/alpha-v1.0.0 'HTTP/1.1 404 Not Found' '{"message":"Not Found"}'
gh_fixture repos/o/r/git/refs/tags/alpha-v1.0.0 'HTTP/2.0 200 OK' \
  '[{"ref":"refs/tags/alpha-v1.0.0X","object":{"type":"tag","sha":"abc"}}]'
tag_ok alpha-v1.0.0X
expect 1 "only a longer tag with the same prefix exists" "alpha-v1.0.0"

new_case metadata-missing
export RELEASE_GAPS_METADATA="${FAKE_DIR}/no-such-file.json"
expect 2 "metadata cannot be read" "could not check"

new_case metadata-bad
printf 'not json' > "${FAKE_DIR}/metadata.json"
expect 2 "metadata is not JSON" "could not check"

new_case no-publishable
write_metadata "${FAKE_DIR}/metadata.json" 'internal 0.0.0 []'
expect 2 "zero publishable packages" "no publishable"

new_case tag-name-config
echo 'git_tag_name = "x"' >> "${FAKE_DIR}/release-plz.toml"
expect 2 "release-plz.toml sets git_tag_name" "git_tag_name"

new_case tag-enable-config
printf '[[package]]\nname = "alpha"\n  git_release_enable = false\n' >> "${FAKE_DIR}/release-plz.toml"
expect 2 "release-plz.toml sets git_release_enable" "git_release_enable"

new_case tag-name-comment
echo '# git_tag_name is not set, so the default format applies.' >> "${FAKE_DIR}/release-plz.toml"
expect 0 "a comment that names git_tag_name is not a setting" "no gaps"

new_case config-missing
export RELEASE_GAPS_CONFIG="${FAKE_DIR}/no-such-file.toml"
expect 2 "release-plz.toml cannot be read" "could not check"

new_case green-unknown
export RELEASE_STEP_OUTCOME=success
crate_fixture alpha 500 'Internal Server Error'
expect 0 "green release step, could-not-check only" "::warning::" "could not check"

new_case red-unknown
export RELEASE_STEP_OUTCOME=failure
crate_fixture alpha 500 'Internal Server Error'
expect 2 "red release step, could-not-check only" "could not check"

new_case green-gap
export RELEASE_STEP_OUTCOME=success
gh_fixture repos/o/r/git/ref/tags/alpha-v1.0.0 'HTTP/1.1 404 Not Found' '{"message":"Not Found"}'
expect 1 "green release step with a gap" "alpha"

new_case cancelled-gap
export RELEASE_STEP_OUTCOME=cancelled
gh_fixture repos/o/r/git/ref/tags/alpha-v1.0.0 'HTTP/1.1 404 Not Found' '{"message":"Not Found"}'
expect 1 "cancelled release step with a gap" "alpha"

new_case summary
export GITHUB_STEP_SUMMARY="${FAKE_DIR}/summary.md"
gh_fixture repos/o/r/git/ref/tags/alpha-v1.0.0 'HTTP/1.1 404 Not Found' '{"message":"Not Found"}'
gh_fixture repos/o/r/releases/tags/alpha-v1.0.0 'HTTP/2 404' '{"message":"Not Found"}'
expect 1 "gap with a job summary" "alpha" "gh release create alpha-v1.0.0" \
  "crates/alpha/CHANGELOG.md" "gh auth refresh -s workflow" "object=deadbeef"
check "gap with a job summary" "summary names the crate" \
  contains "${GITHUB_STEP_SUMMARY}" "alpha"
check "gap with a job summary" "summary has the recovery commands" \
  contains "${GITHUB_STEP_SUMMARY}" "gh api -X POST"
check "gap with a job summary" "summary has no workflow command" \
  no_error_in "${GITHUB_STEP_SUMMARY}"
check "gap with a job summary" "stdout has exactly one ::error:: line" \
  one_error_line "${FAKE_DIR}/stdout.txt"

# Static guards on the real release-plz.yml. No PR runs that workflow, so
# these guards are the only check of its shape before a merge (SMA-745).
release_workflow="${repo_root}/.github/workflows/release-plz.yml"

# Check that the release workflow contains a fixed string.
#   guard <fixed-string>
guard() {
  if grep -qF -- "$1" "${release_workflow}"; then
    echo "ok    — release-plz.yml contains '$1'"
  else
    echo "FAIL  — release-plz.yml contains '$1': string not found" >&2
    failures=$((failures + 1))
  fi
}

guard 'permission-workflows: write'
guard 'environment: release'
guard 'id: release'
guard 'scripts/check-release-gaps.sh'
guard 'RELEASE_GAPS_METADATA'
# The literal ${{ }} is the GitHub expression, not a shell expansion.
# shellcheck disable=SC2016
guard 'GH_TOKEN: ${{ github.token }}'

echo
if [[ "${failures}" == "0" ]]; then
  echo "check-release-gaps selftest: all cases passed"
  exit 0
fi
echo "check-release-gaps selftest: ${failures} case(s) failed" >&2
exit 1
