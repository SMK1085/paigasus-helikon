#!/usr/bin/env bash
# check-release-gaps.sh — find published crate versions that have no git tag or
# no GitHub release (SMA-745).
#
# A release-plz run can publish a crate to crates.io and then fail before it
# makes the tag or the GitHub release. This script finds that gap. It reports
# only. It does not repair a gap.
#
# For each workspace package whose `publish` field is not [], the script checks
# the package version at this commit:
#   1. crates.io lists the version (a yanked version also counts). If not, the
#      version is not published and there is no gap.
#   2. The tag <package>-v<version> exists (exact match).
#   3. A GitHub release that is not a draft exists for that tag.
#
# Results and exit codes:
#   0  no gaps.
#   1  at least one gap (with or without "could not check" items).
#   2  "could not check", no gap. When RELEASE_STEP_OUTCOME is "success", the
#      script writes a ::warning:: and exits 0 instead.
# An error, an unexpected response or an empty package list is always "could
# not check". It is never "no gaps", and never "missing".
#
# Environment inputs:
#   GH_TOKEN               token for `gh` (read access to refs and releases).
#   GITHUB_REPOSITORY      owner/name. Default: SMK1085/paigasus-helikon.
#   RELEASE_GAPS_METADATA  path to `cargo metadata --no-deps --format-version 1`
#                          output. If unset, the script runs that command.
#   RELEASE_GAPS_CONFIG    path to release-plz.toml. Default: <repo>/release-plz.toml.
#   RELEASE_GAPS_SLEEP     seconds between crates.io requests. Default: 1.
#   RELEASE_STEP_OUTCOME   success, failure or cancelled. Unset means failure.
#   GITHUB_SHA             commit for the recovery text. Default: HEAD.
#   GITHUB_STEP_SUMMARY    if set, the report is also appended to this file.
#
# Run locally: GH_TOKEN=$(gh auth token) bash scripts/check-release-gaps.sh

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repo="${GITHUB_REPOSITORY:-SMK1085/paigasus-helikon}"
config="${RELEASE_GAPS_CONFIG:-${repo_root}/release-plz.toml}"
pause="${RELEASE_GAPS_SLEEP:-1}"
outcome="${RELEASE_STEP_OUTCOME:-failure}"
if [[ -n "${GITHUB_SHA:-}" ]]; then
  sha="${GITHUB_SHA}"
else
  sha="$(git -C "${repo_root}" rev-parse HEAD 2>/dev/null || echo HEAD)"
fi
# crates.io refuses the default curl User-Agent with 403.
user_agent="paigasus-helikon-release-gaps (https://github.com/${repo})"

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

gaps="${tmp}/gaps.md"        # one block per gap, for the report.
unknown="${tmp}/unknown.md"  # one line per "could not check" item.
: > "${gaps}"
: > "${unknown}"
gap_count=0
checked=0

# Record an item that the script could not check.
#   could_not_check <text>
could_not_check() {
  printf -- '- could not check: %s\n' "$1" >> "${unknown}"
}

# Print the report to stdout and to the job summary, then exit.
finish() {
  local report="${tmp}/report.md" unknown_count status
  unknown_count="$(grep -c '^- ' "${unknown}" || true)"
  {
    echo "## Release gap report"
    echo
    echo "Repository \`${repo}\`, commit \`${sha}\`."
    echo
    if [[ "${gap_count}" == 0 && "${unknown_count}" == 0 ]]; then
      echo "Result: no gaps. ${checked} published package version(s) checked."
    fi
    if [[ "${gap_count}" != 0 ]]; then
      echo "### Gaps: ${gap_count} published version(s) without tag or release"
      echo
      echo "The recovery commands need a token with the \`workflow\` scope:"
      echo "\`gh auth refresh -s workflow\`. Remove the scope after the recovery:"
      echo "\`gh auth refresh -r workflow\`."
      echo
      cat "${gaps}"
    fi
    if [[ "${unknown_count}" != 0 ]]; then
      echo "### Could not check: ${unknown_count} item(s)"
      echo
      echo "These items are not known to be correct. Do not read them as \"no gaps\"."
      echo
      cat "${unknown}"
      echo
    fi
  } > "${report}"

  cat "${report}"
  # A failed write must not change the exit code: exit 1 means "gap".
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    if ! cat "${report}" >> "${GITHUB_STEP_SUMMARY}" 2>/dev/null; then
      echo "::warning::cannot write job summary"
    fi
  fi

  if [[ "${gap_count}" != 0 ]]; then
    echo "::error::release gaps: ${gap_count} published version(s) without tag or release — see the job summary"
    status=1
  elif [[ "${unknown_count}" != 0 ]]; then
    if [[ "${outcome}" == "success" ]]; then
      echo "::warning::release gap report: could not check ${unknown_count} item(s) — see the job summary"
      status=0
    else
      status=2
    fi
  else
    status=0
  fi
  exit "${status}"
}

# Run `gh api --include <path>` with a time limit. Set `http_status` to the
# status code from the first output line ("" if there is none), and write the
# response body to the file `${tmp}/gh-body`. The gh exit code is ignored: gh
# exits non-zero for every HTTP error, and only the status tells a 404 from an
# auth error.
#   gh_get <api-path>
gh_get() {
  local out="${tmp}/gh-out"
  : > "${out}"
  if command -v timeout >/dev/null 2>&1; then
    timeout 30 gh api --include "$1" > "${out}" 2>"${tmp}/gh-err" || true
  else
    gh api --include "$1" > "${out}" 2>"${tmp}/gh-err" || true
  fi
  tr -d '\r' < "${out}" > "${out}.lf"
  http_status="$(head -n 1 "${out}.lf" | sed -n 's#^HTTP/[0-9.]* \([0-9][0-9][0-9]\).*$#\1#p')"
  # The body starts after the first empty line.
  awk 'body { print } !body && $0 == "" { body = 1 }' "${out}.lf" > "${tmp}/gh-body"
}

# --- tag format guard ----------------------------------------------------------
# The script assumes the default tag format <package>-v<version>. It must not
# guess another format, so a setting that changes tags or releases stops it.
if [[ ! -r "${config}" ]]; then
  could_not_check "cannot read \`${config}\`, so the tag format is not known."
  finish
fi
for key in git_tag_name git_tag_enable git_release_enable; do
  if grep -Eq "^[[:space:]]*${key}[[:space:]]*=" "${config}"; then
    could_not_check "\`${config}\` sets \`${key}\`. This script knows only the default tag format \`<package>-v<version>\` with tags and releases enabled. Update the script for the new setting."
  fi
done
if [[ -s "${unknown}" ]]; then
  finish
fi

# --- package list ----------------------------------------------------------------
metadata="${RELEASE_GAPS_METADATA:-}"
if [[ -z "${metadata}" ]]; then
  metadata="${tmp}/metadata.json"
  if ! cargo metadata --no-deps --format-version 1 \
      --manifest-path "${repo_root}/Cargo.toml" > "${metadata}" 2>"${tmp}/cargo-err"; then
    could_not_check "\`cargo metadata\` failed: $(head -c 300 "${tmp}/cargo-err" | tr '\n' ' ')"
    finish
  fi
fi
if [[ ! -r "${metadata}" ]]; then
  could_not_check "cannot read the metadata file \`${metadata}\`."
  finish
fi
packages="${tmp}/packages.tsv"
# One line per package: name, version, crate directory relative to the
# workspace root (absolute if it is not below the root).
if ! jq -r '
    .workspace_root as $root
    | .packages[]
    | select(.publish != [])
    | (.manifest_path | sub("/Cargo\\.toml$"; "")) as $dir
    | [ .name, .version,
        (if ($dir | startswith($root + "/")) then $dir[($root | length) + 1:] else $dir end)
      ]
    | @tsv' "${metadata}" > "${packages}" 2>"${tmp}/jq-err"; then
  could_not_check "cannot parse the metadata file \`${metadata}\`: $(head -c 300 "${tmp}/jq-err" | tr '\n' ' ')"
  finish
fi
if [[ ! -s "${packages}" ]]; then
  could_not_check "the metadata has no publishable packages (every package has \`publish = []\`), so nothing was checked."
  finish
fi

# --- per package -----------------------------------------------------------------
first_request=1
while IFS=$'\t' read -r name version dir <&3; do
  tag="${name}-v${version}"

  # crates.io. Wait between requests (crawler policy: 1 request per second).
  if [[ "${first_request}" == 0 ]]; then
    sleep "${pause}"
  fi
  first_request=0
  body="${tmp}/crate.json"
  : > "${body}"
  curl_rc=0
  code="$(curl -sS --max-time 20 -A "${user_agent}" -o "${body}" -w '%{http_code}' \
    "https://crates.io/api/v1/crates/${name}" 2>"${tmp}/curl-err")" || curl_rc=$?
  if [[ "${curl_rc}" != 0 ]]; then
    could_not_check "\`${name}\` ${version}: the crates.io request failed (curl exit ${curl_rc}, HTTP ${code:-none}): $(head -c 200 "${tmp}/curl-err" | tr '\n' ' ')"
    continue
  fi
  if [[ "${code}" != "200" ]]; then
    could_not_check "\`${name}\` ${version}: crates.io returned HTTP ${code} for the crate."
    continue
  fi
  if ! listed="$(jq -r --arg v "${version}" '
      if (.versions | type) == "array"
      then ([.versions[].num] | index($v) != null)
      else error("no versions list")
      end' "${body}" 2>/dev/null)"; then
    could_not_check "\`${name}\` ${version}: crates.io returned HTTP 200, but the body has no readable versions list."
    continue
  fi
  # A truncated list can omit the version and make a published version look
  # unpublished. The list length must equal crate.num_versions.
  counts="$(jq -r '"\(.versions | length) \(.crate.num_versions | if type == "number" then . else "missing" end)"' \
    "${body}" 2>/dev/null || echo "unknown unknown")"
  listed_count="${counts%% *}"
  total_count="${counts##* }"
  if [[ "${listed_count}" != "${total_count}" ]]; then
    could_not_check "\`${name}\` ${version}: crates.io lists ${listed_count} version(s), but crate.num_versions is ${total_count}. The versions list can be incomplete."
    continue
  fi
  if [[ "${listed}" != "true" ]]; then
    # Not published, so a missing tag or release is not a gap.
    continue
  fi
  checked=$((checked + 1))

  missing=""

  # Tag. The singular "ref" endpoint matches the exact name only.
  gh_get "repos/${repo}/git/ref/tags/${tag}"
  case "${http_status}" in
    200) ;;
    404) missing="tag" ;;
    "") could_not_check "\`${name}\` ${version}: the tag request for \`${tag}\` gave no HTTP status." ;;
    *) could_not_check "\`${name}\` ${version}: the tag request for \`${tag}\` returned HTTP ${http_status}." ;;
  esac

  # Release.
  gh_get "repos/${repo}/releases/tags/${tag}"
  case "${http_status}" in
    200)
      draft="$(jq -r 'if (.draft | type) == "boolean" then .draft else error("no draft field") end' \
        "${tmp}/gh-body" 2>/dev/null || echo unknown)"
      case "${draft}" in
        false) ;;
        true) missing="${missing:+${missing} }draft-release" ;;
        *) could_not_check "\`${name}\` ${version}: the release request for \`${tag}\` returned HTTP 200, but the body has no readable \`draft\` field." ;;
      esac
      ;;
    404) missing="${missing:+${missing} }release" ;;
    "") could_not_check "\`${name}\` ${version}: the release request for \`${tag}\` gave no HTTP status." ;;
    *) could_not_check "\`${name}\` ${version}: the release request for \`${tag}\` returned HTTP ${http_status}." ;;
  esac

  if [[ -z "${missing}" ]]; then
    continue
  fi
  gap_count=$((gap_count + 1))

  {
    what=""
    for item in ${missing}; do
      case "${item}" in
        tag) what="${what:+${what}, }tag \`${tag}\` is missing" ;;
        release) what="${what:+${what}, }GitHub release \`${tag}\` is missing" ;;
        draft-release) what="${what:+${what}, }GitHub release \`${tag}\` is a draft" ;;
      esac
    done
    echo "#### \`${name}\` ${version}: ${what}"
    echo
    case " ${missing} " in
      *" tag "*)
        echo "Make the tag. release-plz makes the tag object before the ref, so first"
        echo "look for the tag object SHA in the log of the failed run. If you find it,"
        echo "skip the first command. Else make a new annotated tag object. \`${sha}\` is"
        echo "the commit of this run: make sure that it is the commit that published"
        echo "${name} ${version}."
        echo
        echo '```bash'
        echo "gh api -X POST repos/${repo}/git/tags -f tag=${tag} -f message=${tag} -f object=${sha} -f type=commit --jq .sha"
        echo "gh api -X POST repos/${repo}/git/refs -f ref=refs/tags/${tag} -f sha=<tag-object-sha>"
        echo '```'
        echo
        ;;
    esac
    case " ${missing} " in
      *" draft-release "*)
        echo "Publish the draft release, or delete it and make it again with the command"
        echo "below."
        echo
        ;;
    esac
    case " ${missing} " in
      *" release "*|*" draft-release "*)
        echo "Make the release. Use the ${version} section of \`${dir}/CHANGELOG.md\` as"
        echo "the notes file. The tag must exist first. GitHub returns 404 for a draft"
        echo "release, so first look for a draft with this tag in \`gh release list\`."
        echo "If there is one, publish it with \`gh release edit ${tag} --draft=false\`."
        echo "Do not make a second release."
        echo
        echo '```bash'
        echo "gh release create ${tag} --verify-tag --latest=false --title ${tag} --notes-file <file with the ${version} section of ${dir}/CHANGELOG.md>"
        echo '```'
        echo
        ;;
    esac
  } >> "${gaps}"
done 3< "${packages}"

finish
