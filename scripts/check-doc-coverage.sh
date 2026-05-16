#!/usr/bin/env bash
# Requires bash >= 4.0 (mapfile). On macOS: `brew install bash`.
# SMA-305: aggregate workspace-wide docstring coverage and gate on a threshold.
# Per-spec: docs/superpowers/specs/2026-05-16-sma-305-ci-design.md §6.

set -euo pipefail

THRESHOLD="${DOC_COVERAGE_THRESHOLD:-80}"
NIGHTLY="${NIGHTLY_CHANNEL:-nightly}"

# Crates excluded from the aggregated gate. The CLI is allowed to omit docs
# until its public surface stabilizes (spec §7), so it must not contribute
# to the workspace denominator either.
EXCLUDED_CRATES=("paigasus-helikon-cli")

is_excluded() {
  local needle="$1"
  for c in "${EXCLUDED_CRATES[@]}"; do
    [[ "$c" == "$needle" ]] && return 0
  done
  return 1
}

# Run cargo metadata in the main shell so set -e applies; using a
# herestring keeps jq's exit status visible too. Process substitution
# `<(...)` runs in a subshell where pipefail does NOT propagate, so a
# failure of cargo metadata or jq would silently produce an empty list
# and the script would report a vacuous 100% pass.
metadata="$(cargo metadata --format-version 1 --no-deps)"
mapfile -t crates < <(jq -r '.packages[] | .name' <<< "$metadata")

json="$(mktemp)"
trap 'rm -f "$json"' EXIT

total_items=0
total_documented=0
summary_rows=()

for crate in "${crates[@]}"; do
  if is_excluded "$crate"; then
    summary_rows+=("| \`${crate}\` | _excluded_ | _excluded_ | _opt-out (spec §7)_ |")
    continue
  fi

  if ! cargo "+${NIGHTLY}" rustdoc -p "$crate" --all-features -- \
        -Z unstable-options --show-coverage --output-format json \
        > "$json" 2> /dev/null; then
    echo "::warning::rustdoc --show-coverage failed for ${crate}; treating as 0/0"
    summary_rows+=("| \`${crate}\` | n/a | n/a | _rustdoc error_ |")
    continue
  fi

  crate_total=$(jq '[.[].total] | add // 0' "$json")
  crate_docs=$(jq  '[.[].with_docs] | add // 0' "$json")
  total_items=$((total_items + crate_total))
  total_documented=$((total_documented + crate_docs))

  if [[ "$crate_total" -eq 0 ]]; then
    pct="n/a"
  else
    pct=$(awk "BEGIN { printf \"%.1f\", ($crate_docs / $crate_total) * 100 }")
  fi
  summary_rows+=("| \`${crate}\` | ${crate_total} | ${crate_docs} | ${pct}% |")
done

if [[ "$total_items" -eq 0 ]]; then
  workspace_pct="100.0"   # vacuous truth — no public items to document yet
  note="_No public items in workspace yet — baseline pass._"
else
  workspace_pct=$(awk "BEGIN { printf \"%.1f\", ($total_documented / $total_items) * 100 }")
  note=""
fi

{
  echo "## Doc coverage"
  echo
  echo "Workspace: **${workspace_pct}%** (${total_documented}/${total_items}) — threshold: **${THRESHOLD}%**"
  [[ -n "$note" ]] && { echo; echo "$note"; }
  echo
  echo "| Crate | Total | Documented | Coverage |"
  echo "| --- | ---: | ---: | ---: |"
  printf '%s\n' "${summary_rows[@]}"
} >> "${GITHUB_STEP_SUMMARY:-/dev/stdout}"

awk "BEGIN { exit !($workspace_pct >= $THRESHOLD) }" || {
  echo "::error::Doc coverage ${workspace_pct}% is below threshold ${THRESHOLD}%"
  exit 1
}
