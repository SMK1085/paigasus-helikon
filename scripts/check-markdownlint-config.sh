#!/usr/bin/env bash
#
# Asserts that .markdownlint-cli2.jsonc is actually in force.
#
# What this file DOES assert, each as a distinct check with its own failure
# message naming the cause:
#
#   - Several areas the config is supposed to gate are each independently
#     linted: a deep book path, a crate README location, and the repo root.
#     Each is probed separately so a glob change that narrows to (or widens
#     from) any ONE subtree -- e.g. adding "!crates/**", which would leave the
#     book tree and the lint step both green while ungating every crate
#     README -- is caught by name, not just proven for one path.
#   - An explicitly excluded area (docs/superpowers/) is NOT linted.
#   - The explicitly configured MD060 rule value is in force (its "style"
#     option is not silently invalid).
#   - Rules that are only on by virtue of "default": true (MD012, MD040) are
#     still firing. This matters because a "default": false regression does
#     NOT change which files appear in the output -- the explicitly configured
#     MD060 rule still fires on every probe -- so a membership-only check
#     ("does the probe path appear at all?") cannot catch it. A dedicated,
#     by-name check for a default-on rule can.
#   - The membership check itself is tied to a named rule (MD012) that is not
#     MD060, so an MD060-only match can never be misread as proof that the
#     glob set covers the probe (see the "default": false case above: MD060
#     alone would still make the path appear even with default rules off).
#
# What this does NOT prove: that every gated surface in the real glob tree is
# covered (only the probed ones are), or that an unrelated/unknown rule option
# is silently ignored (that failure mode is fail-safe -- the real rule stays
# enabled -- and was verified separately; see the design doc).
#
# Mechanism: markdownlint-cli2 has no --list-files, and per-file lines appear
# only for files WITH findings. So we write probe files that deliberately
# violate three rules at once and assert on whether -- and via which rule IDs
# -- each probe path is reported.
#
# Assertions are positive markers (grep for an expected string), never
# absence-of-findings -- an empty result must never be able to pass.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

EXPECTED_VERSION="0.23.2"

# The probe deliberately violates THREE rules:
#
#   - MD012 (multiple consecutive blank lines) -- always on under "default":
#     true and unaffected by any config this repo sets. Used as the
#     membership marker: a rule ID other than MD060, so this check cannot be
#     satisfied by an MD060 line alone.
#   - MD040 (fenced code block with no language specified) -- also always on
#     under "default": true and otherwise untouched by this repo's config.
#     Its only job is to prove "default": true is actually in force: with
#     "default": false, MD012 and MD040 both stop firing while the
#     explicitly-configured MD060 keeps firing, so the probe path would still
#     appear in the output with neither of these two markers on it.
#   - MD060 with an unpadded body row -- valid under "any"/"tight", a
#     violation under "compact". This is what proves the configured style is
#     in force.
PROBE_BODY='# Probe


Body.

```
untagged fence
```

| a | b |
| --- | --- |
|c|d|
'

# Three areas the config is supposed to gate, probed independently so a glob
# edit that narrows to (or widens from) just one of them cannot hide behind
# the other two staying green.
GATED_PROBES=(
  "docs/book/src/__mdlint_probe.md"
  "crates/paigasus-helikon-core/__mdlint_probe.md"
  "__mdlint_probe.md"
)
EXCLUDED_PROBE="docs/superpowers/__mdlint_probe.md"

cleanup() {
  for probe in "${GATED_PROBES[@]}" "$EXCLUDED_PROBE"; do
    rm -f "$probe"
  done
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

# --- Write every probe, then lint once ---------------------------------------
for probe in "${GATED_PROBES[@]}" "$EXCLUDED_PROBE"; do
  mkdir -p "$(dirname "$probe")"
  printf '%s' "$PROBE_BODY" > "$probe"
done

output="$(npx markdownlint-cli2 2>&1 || true)"

# --- Assertions 1-4, per gated probe -----------------------------------------
for probe in "${GATED_PROBES[@]}"; do
  if [[ "$output" != *"$probe"* ]]; then
    fail "gated probe '$probe' was not linted at all -- the glob set has collapsed or narrowed to exclude this area, or 'gitignore' is excluding it"
  fi

  # `grep -F` without `-q` so it consumes all of stdin; see the SIGPIPE note
  # above.
  probe_lines="$(printf '%s\n' "$output" | grep -F "$probe" || true)"

  if [[ "$probe_lines" != *MD012* ]]; then
    fail "MD012 did not fire on '$probe' -- membership was carried only by another rule (e.g. MD060), which cannot substitute for this check: it likely means 'default' is no longer true, since MD012 is a default-on rule with no explicit config in this repo"
  fi

  if [[ "$probe_lines" != *MD040* ]]; then
    fail "MD040 did not fire on '$probe' -- 'default' is no longer true. MD040 is a default-on rule with no explicit config in this repo, so it only fires while 'default': true is in force; a 'default': false regression disables every rule except the ones explicitly configured (MD013, MD060), and does not change which files appear in the output, so this is the only check that catches it"
  fi

  if [[ "$probe_lines" != *MD060* ]]; then
    fail "MD060 did not fire on '$probe' -- the rule's 'style' value is not in force. An invalid value (not one of aligned/any/compact/tight) disables the rule silently."
  fi
done
echo "ok: all gated probes (deep book path, a crate README location, and the repo root) are linted, with MD012/MD040 (default-on) and MD060 (explicitly configured) all firing"

# --- Assertion 5: the exclusion is in force ----------------------------------
if [[ "$output" == *"$EXCLUDED_PROBE"* ]]; then
  fail "excluded probe '$EXCLUDED_PROBE' WAS linted -- the docs/superpowers exclusion is not in force"
fi
echo "ok: docs/superpowers/ is excluded"

echo "markdownlint config self-test passed"
