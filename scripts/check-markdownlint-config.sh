#!/usr/bin/env bash
#
# Asserts that .markdownlint-cli2.jsonc is actually in force.
#
# Two silent-success modes are guarded, both observed while designing SMA-581:
#
#   1. An INVALID rule-option value silently disables the rule. A typo'd
#      "MD060": { "style": "consistent" } -- not one of aligned/any/compact/tight
#      -- yields "Summary: 0 issues in 0 files" with no error or warning. The
#      gate then reports green while enforcing nothing.
#
#   2. The gated file set can silently collapse or widen. A --no-globs flag, an
#      edit to "globs", or a lost "gitignore" setting changes what is linted with
#      no signal at all.
#
# Mechanism: markdownlint-cli2 has no --list-files, and per-file lines appear
# only for files WITH findings. So we write probe files that deliberately violate
# MD060-compact and assert on whether each is reported.
#
# Assertions are positive markers (grep for an expected string), never
# absence-of-findings -- an empty result must never be able to pass.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

EXPECTED_VERSION="0.23.2"

# The probe deliberately violates TWO rules:
#
#   - MD012 (multiple consecutive blank lines) -- always on under "default": true
#     and unaffected by any config this repo sets. Its only job is to make the
#     probe file APPEAR in the output, which is how we detect glob membership.
#   - MD060 with an unpadded body row -- valid under "any"/"tight", a violation
#     under "compact". This is what proves the configured style is in force.
#
# Two rules, not one, so the two failure modes stay distinguishable: if only the
# MD060 assertion fails we know the rule value is wrong, not the globs.
PROBE_BODY='# Probe


Body.

| a | b |
| --- | --- |
|c|d|
'

GATED_PROBE="docs/book/src/__mdlint_probe.md"
EXCLUDED_PROBE="docs/superpowers/__mdlint_probe.md"

cleanup() {
  rm -f "$GATED_PROBE" "$EXCLUDED_PROBE"
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# --- Assertion 0: we are testing the same binary the gate runs ---------------
# markdownlint-cli2 prints "markdownlint-cli2 vX.Y.Z (markdownlint vA.B.C)" as
# the first line of every run. If the self-test certified a different version
# than the gate, it would prove nothing about the gate.
# NB: every assertion below uses bash pattern matching, NOT `... | grep -q`.
# Under `set -o pipefail`, `grep -q` exits on its FIRST match and closes the
# pipe; the writer then takes SIGPIPE and the pipeline returns 141, which reads
# as "not found". That misfires only when the output is large -- i.e. exactly
# when the repo is dirty and the guard matters most. Observed for real while
# validating this script.
#
# Also match the WHOLE help output, not just line 1: npx prepends its own
# "npm warn exec ..." line when the package is not already installed locally.
help_output="$(npx markdownlint-cli2 --help 2>&1 || true)"
if [[ "$help_output" != *"markdownlint-cli2 v${EXPECTED_VERSION}"* ]]; then
  fail "expected markdownlint-cli2 v${EXPECTED_VERSION}; got:
${help_output}"
fi
echo "ok: linter is v${EXPECTED_VERSION}"

# --- Assertions 1 and 2: probe the gated tree -------------------------------
# A violation in a DEEP gated path must be reported. This proves two things at
# once: the glob recurses (it has not collapsed to the repo root), and the
# MD060 "style" value is honoured rather than silently ignored.
mkdir -p "$(dirname "$GATED_PROBE")"
printf '%s' "$PROBE_BODY" > "$GATED_PROBE"

output="$(npx markdownlint-cli2 2>&1 || true)"

# Membership: does the probe appear at all? Carried by MD012, which no config
# here touches -- so this assertion is about the GLOBS and nothing else.
if [[ "$output" != *"$GATED_PROBE"* ]]; then
  fail "gated probe '$GATED_PROBE' was not linted at all -- the glob set has collapsed or 'gitignore' is excluding it"
fi
echo "ok: deep gated paths are linted"

# Rule value: does MD060 fire *on that same file*? Asserted separately from
# membership so an invalid "style" value cannot be misreported as a glob fault.
# `grep -F` without `-q` so it consumes all of stdin; see the SIGPIPE note above.
probe_lines="$(printf '%s\n' "$output" | grep -F "$GATED_PROBE" || true)"
if [[ "$probe_lines" != *MD060* ]]; then
  fail "MD060 did not fire on '$GATED_PROBE' -- the rule's 'style' value is not in force. An invalid value (not one of aligned/any/compact/tight) disables the rule silently."
fi
echo "ok: MD060.style is in force"

# --- Assertion 3: the exclusion is in force ---------------------------------
# The same violation in an EXCLUDED tree must NOT be reported.
mkdir -p "$(dirname "$EXCLUDED_PROBE")"
printf '%s' "$PROBE_BODY" > "$EXCLUDED_PROBE"

output="$(npx markdownlint-cli2 2>&1 || true)"

if [[ "$output" == *"$EXCLUDED_PROBE"* ]]; then
  fail "excluded probe '$EXCLUDED_PROBE' WAS linted -- the docs/superpowers exclusion is not in force"
fi
echo "ok: docs/superpowers/ is excluded"

echo "markdownlint config self-test passed"
