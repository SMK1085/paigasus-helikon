#!/usr/bin/env bash
#
# Asserts that .markdownlint-cli2.jsonc is actually in force.
#
# What this file DOES assert, each as a distinct check with its own failure
# message naming the cause:
#
#   - Three areas the config is supposed to gate are each independently
#     linted: a deep book path, a crate README location, and the repo root.
#     Each is probed separately, with output lines matched anchored to that
#     exact path (not a bare substring -- see "Anchoring" below), so a glob
#     change that narrows to (or widens from) any ONE subtree -- e.g. adding
#     "!crates/**", which would leave the book tree and the lint step both
#     green while ungating every crate README -- is caught by name, not just
#     proven for one path.
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
# Anchoring (SMA-581 fix-wave-2): output lines are matched with an exact
# path-prefix anchor (`path + ":"` at the START of the line), not a bare
# substring. A bare-substring check is unsound with multiple probes: the repo
# root probe's filename was, until this fix, a plain substring of every other
# probe's path (e.g. "docs/book/src/__mdlint_probe.md" contains
# "__mdlint_probe.md"), so its "membership" and rule checks were silently
# satisfied by the BOOK or CRATE probe's own output lines -- the root leg
# never actually proved anything. Excluding just the root probe from `globs`
# left this script printing "ok: all gated probes ... are linted" and exiting
# 0 while genuinely no longer linting the root. Fixed two ways, deliberately
# redundant: the root probe is named collision-proof
# (`__mdlint_probe_root.md`, not a substring of any sibling path), AND every
# leg's matching is anchored to the exact path so a future rename or a probe
# added later cannot reintroduce the same class of bug.
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

# The MD060 style this repo's .markdownlint-cli2.jsonc is expected to set.
# Asserted by name below so a change to another VALID value cannot pass.
MD060_STYLE="compact"

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
#   - MD060 on a CONSISTENTLY TIGHT table -- valid under "any" and "tight",
#     a violation under "compact". The table must be tight throughout: a
#     table that MIXES styles (e.g. a padded header over an unpadded body)
#     fires MD060 under EVERY style value, so it proves only that some style
#     is set, not which -- a `compact` -> `any` weakening would pass. The
#     assertion below additionally requires the message to NAME "compact",
#     which pins the exact configured value.
PROBE_BODY='# Probe


Body.

```
untagged fence
```

|a|b|
|---|---|
|c|d|
'

# Three areas the config is supposed to gate, probed independently so a glob
# edit that narrows to (or widens from) just one of them cannot hide behind
# the other two staying green. Names are chosen so no probe's path is a
# substring of another's (see "Anchoring" above) -- belt-and-braces alongside
# the anchored matching below, which would catch the collision either way.
GATED_PROBES=(
  "docs/book/src/__mdlint_probe.md"
  "crates/paigasus-helikon-core/__mdlint_probe.md"
  "__mdlint_probe_root.md"
)
EXCLUDED_PROBE="docs/superpowers/__mdlint_probe.md"

# Refuse to run if anything already occupies a probe path. `printf > "$probe"`
# would truncate a real file (or write through a symlink), and cleanup would
# then delete it -- this script must never destroy content it did not create.
# Checked BEFORE the trap is installed, so an abort here removes nothing.
for probe in "${GATED_PROBES[@]}" "$EXCLUDED_PROBE"; do
  if [[ -e "$probe" || -L "$probe" ]]; then
    echo "FAIL: '$probe' already exists; refusing to overwrite it. This path is reserved for a throwaway lint probe -- move or delete it and re-run." >&2
    exit 1
  fi
done

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

# Returns (on stdout) the subset of $1's lines that belong to path $2,
# anchored to the START of the line on the literal string "$2:" -- markdown-
# lint-cli2 emits "path:line[:col] error MDxxx/rule description", so this
# matches only lines that are actually reporting on that exact path, never a
# path for which $2 merely happens to be a substring.
lines_for_path() {
  local all_output="$1"
  local path="$2"
  printf '%s\n' "$all_output" | awk -v p="${path}:" 'index($0, p) == 1'
}

# --- Assertion 0: we are testing the same binary the gate runs ---------------
# markdownlint-cli2 prints "markdownlint-cli2 vX.Y.Z (markdownlint vA.B.C)" as
# the first line of every run. If the self-test certified a different version
# than the gate, it would prove nothing about the gate.
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
  probe_lines="$(lines_for_path "$output" "$probe")"

  if [[ -z "$probe_lines" ]]; then
    fail "gated probe '$probe' was not linted at all -- the glob set has collapsed or narrowed to exclude this area, or 'gitignore' is excluding it"
  fi

  if [[ "$probe_lines" != *MD012* ]]; then
    fail "MD012 did not fire on '$probe' -- membership was carried only by another rule (e.g. MD060), which cannot substitute for this check: it likely means 'default' is no longer true, since MD012 is a default-on rule with no explicit config in this repo"
  fi

  if [[ "$probe_lines" != *MD040* ]]; then
    fail "MD040 did not fire on '$probe' -- 'default' is no longer true. MD040 is a default-on rule with no explicit config in this repo, so it only fires while 'default': true is in force; a 'default': false regression disables every rule except the ones explicitly configured (MD013, MD060), and does not change which files appear in the output, so this is the only check that catches it"
  fi

  # Require the message to name the configured style, not merely to exist.
  # markdownlint-cli2 emits '... for style "<value>"', so this pins the exact
  # value: an INVALID value (not one of aligned/any/compact/tight) disables
  # the rule silently, and a VALID but weaker one ("any"/"tight") stops it
  # firing on the tight probe altogether. Both are caught here.
  if [[ "$probe_lines" != *"MD060"* ]]; then
    fail "MD060 did not fire on '$probe' -- the rule's 'style' value is not in force. An invalid value (not one of aligned/any/compact/tight) disables the rule silently, and the valid-but-weaker values 'any'/'tight' accept the tight probe table."
  fi

  if [[ "$probe_lines" != *"for style \"${MD060_STYLE}\""* ]]; then
    fail "MD060 fired on '$probe' but not for style '${MD060_STYLE}' -- the configured style has been changed to another valid value, which silently changes what the gate enforces"
  fi
done
echo "ok: all gated probes (deep book path, a crate README location, and the repo root) are linted, with MD012/MD040 (default-on) and MD060 (explicitly configured) all firing"

# --- Assertion 5: the exclusion is in force ----------------------------------
excluded_lines="$(lines_for_path "$output" "$EXCLUDED_PROBE")"
if [[ -n "$excluded_lines" ]]; then
  fail "excluded probe '$EXCLUDED_PROBE' WAS linted -- the docs/superpowers exclusion is not in force"
fi
echo "ok: docs/superpowers/ is excluded"

echo "markdownlint config self-test passed"
