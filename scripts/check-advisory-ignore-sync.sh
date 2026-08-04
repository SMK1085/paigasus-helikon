#!/usr/bin/env bash
# check-advisory-ignore-sync.sh — assert the RustSec advisory ignore lists in
# .cargo/audit.toml and deny.toml are identical.
#
# cargo-audit (the `audit` gate) and cargo-deny (the `deny` gate) each read
# their own policy file. Both are required status checks, and since SMA-479
# both are evaluated daily against the same advisory database — so a one-line
# drift between the two ignore lists surfaces as one gate green and the other
# red on `main`, every day, until someone notices.
#
# Matching is deliberately restricted to *quoted* advisory IDs. Both files
# carry unquoted RUSTSEC-* IDs inside comments, as historical notes about
# entries that were removed; matching those would report permanent false
# agreement and the guard would never fire. Known limitation: a quoted
# advisory ID inside a comment is a false positive. Accepted — see
# docs/superpowers/specs/2026-08-04-sma-479-audit-severity-alignment-design.md

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
audit_toml="${repo_root}/.cargo/audit.toml"
deny_toml="${repo_root}/deny.toml"

for f in "${audit_toml}" "${deny_toml}"; do
  if [[ ! -f "${f}" ]]; then
    echo "error: policy file not found: ${f}" >&2
    exit 2
  fi
done

extract_ids() {
  # `|| true` keeps `set -e` from aborting when a file has zero entries —
  # an empty ignore list is legitimate, and must compare equal to an empty one.
  grep -oE '"RUSTSEC-[0-9]{4}-[0-9]{4}"' "$1" | tr -d '"' | sort -u || true
}

audit_ids="$(extract_ids "${audit_toml}")"
deny_ids="$(extract_ids "${deny_toml}")"

if [[ "${audit_ids}" == "${deny_ids}" ]]; then
  count="$(printf '%s' "${audit_ids}" | grep -c . || true)"
  echo "advisory ignore lists agree (${count} entries)"
  exit 0
fi

{
  echo "error: advisory ignore lists differ between .cargo/audit.toml and deny.toml"
  echo
  only_audit="$(comm -23 <(printf '%s\n' "${audit_ids}") <(printf '%s\n' "${deny_ids}") | grep -c . || true)"
  if [[ "${only_audit}" != "0" ]]; then
    echo "  only in .cargo/audit.toml:"
    comm -23 <(printf '%s\n' "${audit_ids}") <(printf '%s\n' "${deny_ids}") | sed 's/^/    /'
  fi
  only_deny="$(comm -13 <(printf '%s\n' "${audit_ids}") <(printf '%s\n' "${deny_ids}") | grep -c . || true)"
  if [[ "${only_deny}" != "0" ]]; then
    echo "  only in deny.toml:"
    comm -13 <(printf '%s\n' "${audit_ids}") <(printf '%s\n' "${deny_ids}") | sed 's/^/    /'
  fi
  echo
  echo "Both files must carry the same [advisories].ignore entries."
  echo "See CONTRIBUTING.md -> Supply-chain security."
} >&2

exit 1
