# markdownlint-cli2 CI Gate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a required `markdown-lint` CI gate over every published Markdown surface, and make the repo clean against it.

**Architecture:** A repo-root `.markdownlint-cli2.jsonc` owns the gated file set and the rule policy, so a bare local run and CI lint an identical set. The linter is pinned by a committed npm lockfile (not a GitHub Action, whose version *is* the linter version). A self-test script asserts the config is actually in force, guarding two silent-success modes. The job lives in `ci.yml`.

**Tech Stack:** `markdownlint-cli2` 0.23.2 (Node >= 22), npm lockfile, GitHub Actions, bash.

**Spec:** `docs/superpowers/specs/2026-08-22-sma-581-markdownlint-ci-gate-design.md`

## Global Constraints

- **Worktree root** is `/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/851df2c8-94b1-4119-8ab2-ab939461c13e/scratchpad/wt-sma-581`. All paths below are relative to it. Use worktree-absolute paths with Write/Edit or files land in the wrong checkout.
- **Never run branch-moving git** (`checkout`, `switch`, `reset --hard`, `rebase`). The worktree shares its object store with other checkouts.
- **Linter version is exactly `0.23.2`.** Do not use `^` or `~` ranges anywhere.
- **Node version in CI is `'24'`** (quoted string; `markdownlint-cli2@0.23.2` declares `engines.node >= 22`).
- **Commit format:** `<type>(<scope>): SMA-581 <lowercase subject>`. Allowed scopes used here: `ci`, `workflows`, `docs`, `readme`, `contributing`, `claude`, `repo`, `runtime-axum`, `runtime-actix`, `runtime-temporal`, `tools`, `plan`. `docs(plans)` is **rejected** by the commit-msg hook — use `docs(plan)`.
- **Every commit is signed** via a 1Password SSH key. If a commit fails with "failed to fill whole buffer", stop and ask the user to unlock their vault. Never bypass signing.
- **Never `git add -A`.** `.env` is untracked-but-not-ignored. Stage explicit paths and verify with `git show --stat`.
- **Never disable a markdownlint rule repo-wide to make a finding go away.** Only `MD013: false` is permitted. If a heading fix produces a new `MD001`, adjust the heading level.
- **Run all cargo/npm commands in the foreground.** Do not background them and end your turn.

---

### Task 1: Toolchain, config, and ignore rules

**Files:**
- Create: `package.json`
- Create: `package-lock.json` (generated, committed)
- Create: `.markdownlint-cli2.jsonc`
- Modify: `.gitignore` (append `node_modules/`)
- Modify: `.gitattributes` (append `*.md text eol=lf`)

**Interfaces:**
- Consumes: nothing.
- Produces: a working `npx markdownlint-cli2` that reads `.markdownlint-cli2.jsonc` and lints exactly 51 files. Tasks 2-4 depend on this exact file set and on the `MD060: compact` rule value.

- [ ] **Step 1: Create `package.json`**

This is a tooling-only manifest for a Rust repo. `"private": true` prevents any accidental publish to npm.

```json
{
  "name": "paigasus-helikon-tooling",
  "version": "0.0.0",
  "private": true,
  "description": "Markdown lint tooling for the paigasus-helikon workspace. Not published; not part of the Rust build.",
  "devDependencies": {
    "markdownlint-cli2": "0.23.2"
  }
}
```

- [ ] **Step 2: Generate the lockfile**

Run from the worktree root:

```bash
npm install
```

Expected: creates `package-lock.json` and `node_modules/`. Confirm the pin resolved exactly:

```bash
node -e "console.log(require('./package-lock.json').packages['node_modules/markdownlint-cli2'].version)"
```

Expected output: `0.23.2`

- [ ] **Step 3: Add `node_modules/` to `.gitignore`**

`node_modules/` is currently **not** ignored. Append to the end of `.gitignore`:

```gitignore
# npm tooling deps (markdown lint) — the lockfile is committed, the tree is not
node_modules/
```

- [ ] **Step 4: Add the Markdown line-ending rule to `.gitattributes`**

Contributors will run `markdownlint-cli2 --fix` locally; on Windows that rewrites files with CRLF, producing whole-file diffs and a local/CI mismatch. Append to `.gitattributes`:

```gitattributes
# Markdown is linted and auto-fixed by markdownlint-cli2; a Windows --fix would
# otherwise rewrite every file with CRLF and diverge from the ubuntu-only CI gate.
*.md text eol=lf
```

- [ ] **Step 5: Create `.markdownlint-cli2.jsonc`**

```jsonc
{
  // Single source of truth for the gated file set. CI passes NO glob arguments,
  // so a bare `npx markdownlint-cli2` locally lints exactly what CI lints.
  //
  // "gitignore" is load-bearing: without it a bare run in a developer checkout
  // picks up untracked-but-ignored trees (measured: 121 files / 616 issues
  // instead of 51 / 110) and the documented local gate becomes unusable.
  "gitignore": true,

  // The explicit negations are kept alongside "gitignore" and are unanchored on
  // purpose:
  //   - Root-anchored "!target/**" would not exclude nested copies, and
  //     target/package/ (cargo package, release-plz verify) holds full crate
  //     copies including README.md.
  //   - .superpowers/ is excluded on some machines only via .git/info/exclude,
  //     which is machine-local and uncommitted, so "gitignore" alone is not
  //     reproducible across contributors.
  "globs": [
    "**/*.md",
    "!docs/superpowers/**",
    "!crates/*/CHANGELOG.md",
    "!**/target/**",
    "!**/node_modules/**",
    "!**/.claude/**",
    "!**/.superpowers/**"
  ],

  "config": {
    "default": true,

    // Line length. 818 of 928 raw findings, and it does not affect rendering.
    // This is the ONLY rule disabled repo-wide.
    "MD013": false,

    // Table pipe style. "compact" is the `| --- | --- |` single-space form this
    // repo already predominantly writes; the rule default is "any" (66 findings)
    // and "aligned" would be 912 and force re-padding on every table edit.
    //
    // WARNING: an INVALID value here (e.g. "consistent", which is not one of
    // aligned/any/compact/tight) silently disables the rule and the gate reports
    // green while enforcing nothing. scripts/check-markdownlint-config.sh
    // asserts against exactly that.
    "MD060": { "style": "compact" }
  }
}
```

- [ ] **Step 6: Verify the gated file set**

```bash
npx markdownlint-cli2 2>&1 | grep -E 'Linting|Summary'
```

Expected exactly:

```text
Linting: 51 files
Summary: 110 issues in 16 files
```

If the file count is not 51, the globs are wrong — do not proceed. A count near 121 means `gitignore`/negations are not taking effect; a count near 6 means the globs collapsed to the repo root.

- [ ] **Step 7: Commit**

```bash
git add package.json package-lock.json .markdownlint-cli2.jsonc .gitignore .gitattributes
git show --stat --oneline HEAD  # after committing, confirm ONLY these 5 files
git commit -m "ci(repo): SMA-581 pin markdownlint-cli2 and add lint config"
```

---

### Task 2: Config self-test script

**Files:**
- Create: `scripts/check-markdownlint-config.sh`

**Interfaces:**
- Consumes: `.markdownlint-cli2.jsonc` and the `node_modules/` install from Task 1.
- Produces: `scripts/check-markdownlint-config.sh`, invoked as `bash scripts/check-markdownlint-config.sh` from the repo root. Task 5 adds it as a CI step.

This guards two silent-success modes, both hit while designing this ticket: an invalid rule-option value disables a rule with no error, and the gated file set can silently narrow or widen. `markdownlint-cli2` has no `--list-files`, and per-file output lines appear only for files that *have* findings — so the script writes probe files that deliberately violate `MD060` and asserts on whether they are reported.

- [ ] **Step 1: Write the script**

```bash
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
```

- [ ] **Step 2: Make it executable**

```bash
chmod +x scripts/check-markdownlint-config.sh
```

- [ ] **Step 3: Run it against the real config**

```bash
bash scripts/check-markdownlint-config.sh
```

Expected: four `ok:` lines then `markdownlint config self-test passed`, exit 0.

Note the probes are removed by the `trap` even on failure. Confirm with `git status --short` — it must show no `__mdlint_probe.md`.

- [ ] **Step 4: Mutation-test it — three mutations, each must blame the right cause**

A guard that fails for the wrong reason sends the next person hunting the wrong
thing, so assert on the *message*, not just the exit code.

Use this helper. Two deliberate choices in it: the find/replace strings are
passed as **argv** to a single-quoted Python one-liner (the JSON contains `|`,
`*`, `/` and quotes, which collide with `sed` delimiters and with inline
quoting), and there is **no `| tail`** — `FAIL` goes to stderr unbuffered while
`ok:` goes to stdout block-buffered, so piping interleaves them in a misleading
order.

```bash
cp .markdownlint-cli2.jsonc /tmp/mdlint-backup.jsonc

mutate() {  # mutate "<find>" "<replace>"
  cp /tmp/mdlint-backup.jsonc .markdownlint-cli2.jsonc
  python3 -c 'import sys; p=".markdownlint-cli2.jsonc"; s=open(p).read(); open(p,"w").write(s.replace(sys.argv[1], sys.argv[2]))' "$1" "$2"
  bash scripts/check-markdownlint-config.sh; echo "exit=$?"
}

mutate '"style": "compact"'       '"style": "consistent"'
mutate '"**/*.md",'               '"*.md",'
mutate '"!docs/superpowers/**",'  ''

cp /tmp/mdlint-backup.jsonc .markdownlint-cli2.jsonc
```

Expected — all three `exit=1`, each naming a *different* cause:

```text
M1 invalid rule value:
  ok: linter is v0.23.2
  ok: deep gated paths are linted
  FAIL: MD060 did not fire on 'docs/book/src/__mdlint_probe.md' -- the rule's 'style' value is not in force. ...

M2 collapsed globs:
  ok: linter is v0.23.2
  FAIL: gated probe 'docs/book/src/__mdlint_probe.md' was not linted at all -- the glob set has collapsed ...

M3 lost exclusion:
  ok: linter is v0.23.2
  ok: deep gated paths are linted
  ok: MD060.style is in force
  FAIL: excluded probe 'docs/superpowers/__mdlint_probe.md' WAS linted -- the docs/superpowers exclusion is not in force
```

If any exits 0, the script is not guarding anything. If M1 or M3 reports the M2
message, an assertion is bleeding into another — most likely a `... | grep -q`
has been reintroduced (see the SIGPIPE note in the script).

- [ ] **Step 5: Confirm no probe files leaked**

The `trap` must remove both probes even on failure.

```bash
git status --short | grep __mdlint_probe && echo "LEAKED — fix the trap" || echo "no stray probes"
```

- [ ] **Step 6: Confirm the config is restored**

```bash
git diff --exit-code .markdownlint-cli2.jsonc && echo "config restored"
npx markdownlint-cli2 2>&1 | grep -E 'Linting|Summary'
```

Expected: `config restored`, then `Linting: 51 files` / `Summary: 110 issues in 16 files`.

- [ ] **Step 7: Commit**

```bash
git add scripts/check-markdownlint-config.sh
git commit -m "ci(repo): SMA-581 assert the markdownlint config is in force"
```

---

### Task 3: Apply automatic fixes, minus the one that corrupts meaning

**Files:**
- Modify (auto): `BENCHMARKS.md`, `CLAUDE.md`, `CODE_OF_CONDUCT.md`, `CONTRIBUTING.md`, `SECURITY.md`, `docker/forkd/README.md`, `docs/book/src/concepts/tools.md`, `docs/runbooks/forkd-live-validation.md`, `crates/paigasus-helikon-providers-litellm/README.md`, `crates/paigasus-helikon-runtime-actix/README.md`, `crates/paigasus-helikon-runtime-axum/README.md`, `crates/paigasus-helikon-runtime-temporal/README.md`

**Interfaces:**
- Consumes: the config from Task 1.
- Produces: 12 residual findings for Task 4 to hand-fix.

`--fix` resolves 98 of 110 findings. The diff is **not** purely whitespace — two auto-fixes are content edits, and one of them is wrong.

- [ ] **Step 1: Record the pre-fix state of the one bad auto-fix**

```bash
grep -n 'Bearer' crates/paigasus-helikon-providers-litellm/README.md
```

Expected (note the trailing space inside the backticks — it is the point of the sentence):

```text
85:keys are treated as absent rather than sent as a malformed `Bearer `.
```

- [ ] **Step 2: Run the auto-fixer**

```bash
npx markdownlint-cli2 --fix
```

- [ ] **Step 3: Inspect the two content edits**

```bash
git diff crates/paigasus-helikon-providers-litellm/README.md CODE_OF_CONDUCT.md
```

Expected two hunks:

- `CODE_OF_CONDUCT.md`: `dev@paigasus.com` → `<dev@paigasus.com>` (MD034). **Keep it** — it renders as a mailto link, and this is a filled-in Contributor Covenant placeholder, not verbatim upstream text.
- `crates/.../providers-litellm/README.md`: `` `Bearer ` `` → `` `Bearer` `` (MD038). **This is wrong.** The sentence describes a *malformed* header with a dangling space; deleting the space inverts the meaning.

- [ ] **Step 4: Reword the MD038 site instead of accepting the auto-fix**

In `crates/paigasus-helikon-providers-litellm/README.md`, replace the line so the prose no longer depends on a trailing space inside a code span:

Before (as left by `--fix`):

```markdown
keys are treated as absent rather than sent as a malformed `Bearer`.
```

After:

```markdown
keys are treated as absent rather than sent as a `Bearer` header with an empty
credential.
```

- [ ] **Step 5: Verify the residual count**

```bash
npx markdownlint-cli2 2>&1 | grep -E 'MD[0-9]{3}|Summary'
```

Expected `Summary: 12 issues in 5 files`, and exactly these 12:

```text
.github/PULL_REQUEST_TEMPLATE.md:1 MD041
crates/paigasus-helikon-tools/README.md:9 MD001
docs/book/src/SUMMARY.md:5 MD025
docs/book/src/SUMMARY.md:10 MD025
docs/book/src/SUMMARY.md:25 MD025
docs/book/src/SUMMARY.md:31 MD025
docs/runbooks/agentcore-image-check.md:24 MD028
docs/runbooks/forkd-live-validation.md:323 MD040
docs/runbooks/forkd-live-validation.md:388 MD036
docs/runbooks/forkd-live-validation.md:394 MD036
docs/runbooks/forkd-live-validation.md:400 MD036
docs/runbooks/forkd-live-validation.md:406 MD036
```

- [ ] **Step 6: Confirm the tracing markers were not disturbed**

```bash
git diff --name-only | grep observability-evaluation && echo "UNEXPECTED — investigate" || echo "ok: marker file untouched"
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
```

Expected: `ok: marker file untouched`, then the test passes.

- [ ] **Step 7: Commit**

```bash
git add BENCHMARKS.md CLAUDE.md CODE_OF_CONDUCT.md CONTRIBUTING.md SECURITY.md \
  docker/forkd/README.md docs/book/src/concepts/tools.md \
  docs/runbooks/forkd-live-validation.md \
  crates/paigasus-helikon-providers-litellm/README.md \
  crates/paigasus-helikon-runtime-actix/README.md \
  crates/paigasus-helikon-runtime-axum/README.md \
  crates/paigasus-helikon-runtime-temporal/README.md
git commit -m "docs(repo): SMA-581 apply markdownlint automatic fixes"
```

---

### Task 4: Hand-fix the remaining 12 findings

**Files:**
- Modify: `docs/runbooks/forkd-live-validation.md` (8 heading conversions + 1 fence)
- Modify: `docs/runbooks/agentcore-image-check.md:24`
- Modify: `crates/paigasus-helikon-tools/README.md:9`
- Modify: `docs/book/src/SUMMARY.md` (prepend a scoped disable)
- Modify: `.github/PULL_REQUEST_TEMPLATE.md` (prepend a scoped disable)

**Interfaces:**
- Consumes: the 12 residual findings from Task 3.
- Produces: a tree that passes `npx markdownlint-cli2` with 0 issues. Task 5's CI job depends on this.

- [ ] **Step 1: Convert all eight bold pseudo-headings in the forkd runbook to `###`**

Under `## Troubleshooting` there are **eight** bold-line pseudo-headings, but only four trip `MD036`. Convert **all eight** — converting only the four that fire would leave the section half-converted and inconsistent.

Use `###`, **not** `####`. The nearest preceding heading is `## Troubleshooting` (h2), so `####` is a two-level skip and would introduce four fresh `MD001` violations.

Replace each of these lines in `docs/runbooks/forkd-live-validation.md`:

```markdown
**`forkd doctor` fails: "KVM not available"**
**`forkd from-image` fails with "build-rootfs.sh not found" or similar**
**`forkd` or `forkd-controller` fails with "GLIBC_2.3x not found"**
**`entrypoint.sh` exits with "FATAL: netns … missing FORWARD drop rule"**
**TLS handshake failure in tests**
**Egress-deny test hangs (> 8 seconds)**
**In-netns proxy fails to resolve CONNECT targets**
**Secret scan fails**
```

with the same text as an h3:

```markdown
### `forkd doctor` fails: "KVM not available"
### `forkd from-image` fails with "build-rootfs.sh not found" or similar
### `forkd` or `forkd-controller` fails with "GLIBC_2.3x not found"
### `entrypoint.sh` exits with "FATAL: netns … missing FORWARD drop rule"
### TLS handshake failure in tests
### Egress-deny test hangs (> 8 seconds)
### In-netns proxy fails to resolve CONNECT targets
### Secret scan fails
```

- [ ] **Step 2: Tag the bare fence in the forkd runbook**

At `docs/runbooks/forkd-live-validation.md:323` (under the `### Expected output` heading) there is a bare opening fence holding test output. Change:

```markdown
```
test live_forkd_runs_bash_in_a_microvm ... ok
```

to a `text`-tagged fence:

```markdown
```text
test live_forkd_runs_bash_in_a_microvm ... ok
```

Only the **opening** fence changes; leave the closing fence alone.

- [ ] **Step 3: Separate the two blockquotes in the agentcore runbook**

`docs/runbooks/agentcore-image-check.md` lines 3-23 and 25-40 are two *deliberately separate* blockquotes: a 2026-07-06 local measurement and a 2026-08-07 CI measurement. A bare blank line between them makes Markdown treat them as one quote with a gap (`MD028`).

Do **not** merge them. Replace the blank line at line 24 with an empty HTML comment:

```markdown
> Both gates passed with wide margin (agent image at ~11% of the size budget;
> cold start at ~20% of the latency budget).

<!-- -->

> **CI-observed 2026-08-07 on `ubuntu-24.04-arm`** (native arm64, no
```

- [ ] **Step 4: Fix the heading skip in the tools README**

`crates/paigasus-helikon-tools/README.md` opens with `# paigasus-helikon-tools` (h1) and its next heading at line 9 is `###` — a two-level skip. This is a live crates.io page. Change line 9 from:

```markdown
### microVM egress enforcement (`microvm` feature, SMA-437)
```

to:

```markdown
## microVM egress enforcement (`microvm` feature, SMA-437)
```

Then check the rest of the file for headings that were nested under that one and now sit at the wrong level:

```bash
grep -n '^#\{1,6\} ' crates/paigasus-helikon-tools/README.md
```

If any subsequent heading now skips a level, adjust it. **Never disable `MD001`.**

- [ ] **Step 5: Scope-disable `MD025` in the mdBook summary**

mdBook *requires* multiple top-level headings in `SUMMARY.md` — each `# Title` is a part separator. The rule is wrong for this one file, so disable it there rather than repo-wide.

Prepend to `docs/book/src/SUMMARY.md`:

```markdown
<!-- markdownlint-disable-file MD025 -->
```

followed by a blank line, before the existing `# Summary` line.

(Verified during design: `mdbook build` succeeds with this comment present and the generated `book/html/toc.js` is byte-identical to baseline.)

- [ ] **Step 6: Scope-disable `MD041` in the PR template**

GitHub PR templates conventionally open at `## Summary` with no h1. Prepend to `.github/PULL_REQUEST_TEMPLATE.md`:

```markdown
<!-- markdownlint-disable-file MD041 -->
```

followed by a blank line, before the existing `## Summary` line.

- [ ] **Step 7: Verify the tree is clean**

```bash
npx markdownlint-cli2
```

Expected: `Summary: 0 issues` and exit 0.

If a new `MD001` appeared, a heading level is wrong — fix the level, do not disable the rule.

- [ ] **Step 8: Verify the mdBook still builds identically**

```bash
cd docs/book && mdbook build && cd ../..
```

Expected: build succeeds (linkcheck included). Confirm the sidebar is unchanged:

```bash
git diff --stat docs/book/src/SUMMARY.md
```

Expected: exactly 2 insertions (the comment and a blank line), 0 deletions.

- [ ] **Step 9: Re-run the self-test and the tracing test**

```bash
bash scripts/check-markdownlint-config.sh
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
```

Expected: both pass.

- [ ] **Step 10: Commit**

```bash
git add docs/runbooks/forkd-live-validation.md docs/runbooks/agentcore-image-check.md \
  crates/paigasus-helikon-tools/README.md docs/book/src/SUMMARY.md \
  .github/PULL_REQUEST_TEMPLATE.md
git commit -m "docs(repo): SMA-581 hand-fix the residual markdownlint findings"
```

---

### Task 5: Add the CI job

**Files:**
- Modify: `.github/workflows/ci.yml` (append a `markdown-lint` job)

**Interfaces:**
- Consumes: `.markdownlint-cli2.jsonc`, `package-lock.json`, `scripts/check-markdownlint-config.sh`.
- Produces: a `markdown-lint` status context. Task 6 makes it required.

The job goes in `ci.yml`, **not** `docs.yml`. `docs.yml`'s `book-deploy` only `needs: book-build`, so a lint failure there would redden the `docs` workflow run while Pages still deployed — making "docs is red" ambiguous between "the site failed to publish" and "a heading was wrong".

- [ ] **Step 1: Append the job to `.github/workflows/ci.yml`**

Add as the last entry under `jobs:` (after `sessions-it`), matching the file's existing two-space job indentation:

```yaml
  markdown-lint:
    runs-on: ubuntu-latest
    # Bounds a hung `npm ci`. A required check that sits for six hours is worse
    # than one that fails.
    timeout-minutes: 10
    steps:
      # actions/checkout v7.0.1
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1
        with:
          persist-credentials: false
      # actions/setup-node v7.0.0
      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020
        with:
          node-version: '24'
          cache: npm
      - name: Install pinned markdownlint-cli2
        run: npm ci
      # No glob arguments: .markdownlint-cli2.jsonc's "globs" is the sole source
      # of truth, so this run and a bare local run lint an identical set by
      # construction. Deliberately NOT path-filtered -- a path-filtered REQUIRED
      # check never reports on a PR that touches no Markdown, and blocks it
      # forever (the trap sessions-it avoids with step-level guards).
      - name: Lint Markdown
        run: npx markdownlint-cli2
      # Asserts the config above is actually in force. An invalid rule-option
      # value disables a rule silently, and the gated file set can collapse with
      # no signal -- either would make the step above a green no-op.
      - name: Verify the lint config is in force
        run: bash scripts/check-markdownlint-config.sh
```

- [ ] **Step 2: Confirm the job does not use a path filter**

```bash
grep -n 'paths-filter\|paths:' .github/workflows/ci.yml
```

Expected: matches only inside the pre-existing `sessions-it` job, none inside `markdown-lint`.

- [ ] **Step 3: Validate the workflow YAML**

If `actionlint` is available:

```bash
actionlint .github/workflows/ci.yml
```

Otherwise verify it parses:

```bash
python3 -c "import yaml,sys; d=yaml.safe_load(open('.github/workflows/ci.yml')); print(sorted(d['jobs'].keys()))"
```

Expected: the job list includes `markdown-lint` and has 9 entries.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci(workflows): SMA-581 add the markdown-lint job"
```

---

### Task 6: Make it required, and update the docs

**Files:**
- Modify: `.github/rulesets/main-protection-checks.json`
- Modify: `CONTRIBUTING.md` (required-contexts row ~line 300; local-gates block ~line 227; a facade-README note)
- Modify: `CLAUDE.md` (CI section)

**Interfaces:**
- Consumes: the `markdown-lint` context from Task 5.
- Produces: nothing downstream. This is the last task.

- [ ] **Step 1: Add the required context**

In `.github/rulesets/main-protection-checks.json`, add to the `required_status_checks` array, after the `{ "context": "build-no-default-features" }` entry:

```json
          { "context": "markdown-lint" }
```

Mind the trailing-comma rules — the previous entry now needs a comma, and `markdown-lint` must be last with none.

- [ ] **Step 2: Verify the JSON still parses**

```bash
python3 -m json.tool .github/rulesets/main-protection-checks.json > /dev/null && echo "valid json"
```

- [ ] **Step 3: Update the CONTRIBUTING required-contexts row**

In `CONTRIBUTING.md`, the `.github/rulesets/main-protection-checks.json` table row lists every required context. Append to that list, after the `build-no-default-features` clause:

```markdown
, `markdown-lint` (required because it is the only gate that lints the published Markdown surfaces — the mdBook, every crate README, the repo-root docs, and the runbooks — against `.markdownlint-cli2.jsonc`)
```

- [ ] **Step 4: Add the local gate to CONTRIBUTING**

In the local pre-PR gate block (the fenced `bash` block containing `cargo fmt --all -- --check`), append these two lines at the end of that block:

```bash
npm ci                                               # once, or after package-lock.json changes
npx markdownlint-cli2                                # reads .markdownlint-cli2.jsonc
```

Then add this paragraph immediately after that fenced block:

```markdown
The Markdown gate needs Node (>= 22); everything else in that list needs only the
Rust toolchain. `markdownlint-cli2` is pinned exactly in `package-lock.json` and is
**not** tracked by Dependabot — bumping it is a deliberate act, like `PROTOC_VERSION`
and `NIGHTLY_TOOLCHAIN`. `npx markdownlint-cli2 --fix` resolves most findings
mechanically; review the diff before committing, since a few fixes edit content
rather than whitespace.
```

- [ ] **Step 5: Add the facade-README fence note to CONTRIBUTING**

`crates/paigasus-helikon/README.md` is `include_str!`'d into the facade's rustdoc, so its fenced blocks are compiled as doctests. Now that `MD040` requires every fence to carry a language, add this to CONTRIBUTING's documentation section:

```markdown
In `crates/paigasus-helikon/README.md` specifically, tag prose snippets ` ```text `
or ` ```ignore ` — never a bare ` ```rust `. That file is `include_str!`'d into the
facade's rustdoc, so a `rust` fence becomes a doctest that CI's test gate will run.
```

- [ ] **Step 6: Update the CLAUDE.md CI section**

Two edits.

First, the sentence that opens the CI section currently says `ci.yml` runs eight jobs. Change "eight jobs" to "nine jobs" and add `markdown-lint` to the enumerated list, described as:

```markdown
`markdown-lint` (SMA-581: `markdownlint-cli2` over every published Markdown surface, with the gated file set and rule policy in `.markdownlint-cli2.jsonc`; deliberately not path-filtered, because a path-filtered *required* check never reports on a PR touching no Markdown and blocks it forever)
```

Second, add a paragraph to the CI section recording the pin:

```markdown
**`markdownlint-cli2` is pinned by `package-lock.json`, not by a GitHub Action.** `DavidAnson/markdownlint-cli2-action` was rejected because the action version *is* the linter version, and markdownlint ships new rules in **minor** releases — which Dependabot's `github-actions` group takes. A routine grouped `chore(deps)` PR could therefore redden a *required* gate for reasons nobody chose, and the `branch-names` ruleset blocks humans pushing to `dependabot/**`, so it could not be fixed in place. `npm` is not a configured Dependabot ecosystem, so this joins the repo's hand-bumped pins alongside `PROTOC_VERSION`/`TEMPORAL_CLI_VERSION` in `integration.yml` and `NIGHTLY_TOOLCHAIN` in `ci.yml`. **Two of this tool's failure modes report success:** an invalid rule-option value (e.g. `MD060: { style: "consistent" }`) silently disables the rule, and the gated file set can silently collapse — `scripts/check-markdownlint-config.sh` asserts against both and runs as a step in the job. Do not fold it into the lint step; it must be able to fail independently.
```

Also add the required-contexts list in CLAUDE.md's CI section: append `markdown-lint` to the enumerated bare job names.

- [ ] **Step 7: Verify the docs edits did not break the lint**

The files just edited are themselves gated.

```bash
npx markdownlint-cli2
bash scripts/check-markdownlint-config.sh
```

Expected: `Summary: 0 issues`, self-test passes.

- [ ] **Step 8: Commit**

```bash
git add .github/rulesets/main-protection-checks.json CONTRIBUTING.md CLAUDE.md
git commit -m "docs(contributing): SMA-581 document the markdown-lint required gate"
```

- [ ] **Step 9: Final full verification**

```bash
npx markdownlint-cli2
bash scripts/check-markdownlint-config.sh
cd docs/book && mdbook build && cd ../..
cargo test -p paigasus-helikon-workspace-lints
git status --short
```

Expected: lint 0 issues; self-test passes; mdBook builds; workspace-lints tests pass; working tree clean (no stray `__mdlint_probe.md`, no `node_modules/` shown — it is now ignored).

---

## Post-merge, by the maintainer (NOT part of implementation)

The ruleset JSON edit in Task 6 is **inert on its own**. `scripts/apply-repo-config.sh` is what applies it, and there is no drift-check job.

1. Merge the PR to `main`.
2. Run `bash scripts/apply-repo-config.sh`.
3. Verify **enforcement**, not listing:

```bash
gh api repos/SMK1085/paigasus-helikon/rulesets --jq '.[] | select(.name=="main-protection-checks") | .id'
gh api repos/SMK1085/paigasus-helikon/rulesets/<id> \
  --jq '.rules[] | select(.type=="required_status_checks") | .parameters.required_status_checks[].context'
```

Expect `markdown-lint` in the output.

Rollback: remove the context from the JSON, re-run the script, then remove the job.

**PR #218** will need the new context to report before it can merge. It does *not* need a rebase — `strict_required_status_checks_policy` is `false`. An empty commit, or closing and reopening the PR, re-triggers `ci.yml`.

**Expect a release PR** bumping five crates for mechanical README changes: `providers-litellm`, `runtime-actix`, `runtime-axum`, `runtime-temporal`, `tools`. This is the pure-auto path — release-plz performs the bumps itself and `dependencies_update` cascades to the facade, so no manual `core`/facade bump is needed.

**On the first PR after merge**, confirm CodeRabbit's Markdown comments are still scoped to the diff. `.markdownlint-cli2.jsonc` is auto-discovered by CodeRabbit; if the `**/*.md` glob makes it report across all 51 files, add a `tools.markdownlint` block to `.coderabbit.yaml`.
