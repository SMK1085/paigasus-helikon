#!/usr/bin/env bash
# SMA-822: assert that the sqlx `sqlite` feature (libsqlite3-sys) reaches only
# the crates that use it.
#
#   absent  : sessions-postgres, and the facade with only `sessions-postgres`
#   present : sessions-sqlite, and evals with `trace-sqlite`
#
# The script resolves the dependency graph only. It does not build anything.
#
# Why the absent case reads stderr: when a package is not in the graph,
# cargo 1.99 exits 101 and prints
# ``package ID specification `libsqlite3-sys` did not match any packages``. Older cargo
# exits 0 and prints `nothing to print`. The script accepts both forms.
# Any other failure (for example a broken manifest) does not count as absent.

set -uo pipefail

fail=0
err_file="$(mktemp)"
trap 'rm -f "$err_file"' EXIT

# Run `cargo tree` for libsqlite3-sys. Sets: out, code, err.
run_tree() {
  out="$(cargo tree --color never --target all "$@" -i libsqlite3-sys -e normal,build 2>"$err_file")"
  code=$?
  err="$(cat "$err_file")"
}

report_failure() {
  echo "::error::$1"
  echo "exit code: $code"
  echo "stdout:"
  echo "$out"
  echo "stderr:"
  echo "$err"
  fail=1
}

# Confirm the package selection resolves. With `-i`, cargo ignores an unknown
# `-p` and prints the whole workspace, so a renamed crate would fool both
# assertions. Returns 1 (after reporting) when the selection does not resolve.
package_resolves() {
  local label="$1"
  shift
  local name="" prev=""
  for arg in "$@"; do
    [ "$prev" = "-p" ] && name="$arg"
    prev="$arg"
  done
  out="$(cargo tree --color never --target all -e normal,build --depth 0 "$@" 2>"$err_file")"
  code=$?
  err="$(cat "$err_file")"
  if [ "$code" -eq 0 ] && [[ "$out" == "$name v"* ]]; then
    return 0
  fi
  report_failure "The guard's package selection $label does not resolve (renamed crate or feature?). Fix scripts/check-sqlite-scope.sh."
  return 1
}

assert_absent() {
  local label="$1"
  shift
  package_resolves "$label" "$@" || return 0
  run_tree "$@"
  if [ -z "$out" ] && {
    { [ "$code" -ne 0 ] && [[ "$err" == *"specification \`libsqlite3-sys\` did not match any packages"* ]]; } ||
      { [ "$code" -eq 0 ] && [[ "$err" == *"nothing to print"* ]]; }
  }; then
    echo "ok: SQLite is absent from $label"
  else
    report_failure "SQLite reached $label. Keep \`sqlite\` out of the root \`sqlx\` features and enable it only in the crate that uses it (SMA-822)."
  fi
}

assert_present() {
  local label="$1"
  shift
  package_resolves "$label" "$@" || return 0
  run_tree "$@"
  if [ "$code" -eq 0 ] && [[ "$out" == *"libsqlite3-sys v"* ]]; then
    echo "ok: SQLite is present in $label"
  else
    report_failure "$label lost the sqlx \`sqlite\` feature, or the check cannot find \`libsqlite3-sys\`. Add \`features = [\"sqlite\"]\` to its \`sqlx\` entry."
  fi
}

assert_absent  "sessions-postgres"         -p paigasus-helikon-sessions-postgres
assert_absent  "facade[sessions-postgres]" -p paigasus-helikon --no-default-features --features sessions-postgres
assert_present "sessions-sqlite"           -p paigasus-helikon-sessions-sqlite
assert_present "evals[trace-sqlite]"       -p paigasus-helikon-evals --features trace-sqlite

exit "$fail"
