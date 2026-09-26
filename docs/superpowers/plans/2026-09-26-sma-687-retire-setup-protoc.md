# SMA-687 — Retire `setup-protoc` and the protoc pin — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the last `protoc` install from CI, delete `.github/actions/setup-protoc/`, and update `CLAUDE.md` and the CI runbook.

**Architecture:** Pure CI and documentation change. One workflow step and one local composite action go away. No crate, manifest or lockfile changes.

**Tech Stack:** GitHub Actions YAML, Markdown.

**Spec:** `docs/superpowers/specs/2026-09-26-sma-687-retire-setup-protoc-design.md` (approved at Gate 1). Section numbers below refer to it.

## Global Constraints

- Work only in `/Users/smaschek/dev/paigasus/paigasus-helikon/.worktrees/sma-687`, branch `feature/sma-687-retire-setup-protoc-from-release-plzyml-and-delete-the`. Use absolute paths under that root for every edit.
- Never run `git checkout`, `git switch`, `git reset`, `git stash`, `git rebase` or `git push`. Never `git add -A` or `git add .`; add files by explicit path (`git rm -r` for the deleted directory).
- Do not edit `Cargo.toml`, `Cargo.lock`, any file under `crates/`, `ci.yml`, `integration.yml`, `msrv.yml`, `CONTRIBUTING.md` or `docs/book/`.
- Commit messages: `<type>(<scope>): SMA-687 <lowercase subject>`, types `chore`/`docs` only, scopes from `.versionrc`. End with `Co-Authored-By: <the model doing the work> <noreply@anthropic.com>`. Commits are signed; on a signing error, stop and report.
- New prose uses ASD-STE100 Simplified Technical English.
- The full SHA of the recovery commit is `0d5e80be0bcc2d5854829831e8ecf1f9c1eaf8ed`.
- Scripts under `scripts/` need bash >= 4: run them with `/opt/homebrew/bin/bash`.

## Review Focus

1. **Broken `release-plz.yml` YAML.** A PR never runs this workflow, so a YAML error shows only after the merge, as a failed release. Task 1 parses it and checks the step list.
2. **A pin sentence in `CLAUDE.md` that names a pin that does not exist.** Task 2 greps each named pin first.
3. **Wrong placement of the runbook history paragraph** (it must replace the paragraph before the recovery paragraph). Task 2 checks the section order.
4. **A leftover `setup-protoc`/`PROTOC_VERSION` reference.** Task 3 runs `git grep` over the whole repo.
5. **Lint regressions in the edited Markdown.** Tasks 2 and 3 run `markdownlint-cli2`.

---

### Task 1: Remove the step from `release-plz.yml` and delete the action

**Files:**
- Modify: `.github/workflows/release-plz.yml` (lines 18-22 and 33-39)
- Delete: `.github/actions/setup-protoc/action.yml`, `install.sh`, `verify.sh`, `selftest.sh`

**Interfaces:**
- Consumes: nothing.
- Produces: no file under `.github/actions/`; `release-plz.yml` without a `./.github/actions/` step.

- [ ] **Step 1: Prove the current state (the "failing test")**

```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon/.worktrees/sma-687
grep -n 'setup-protoc' .github/workflows/release-plz.yml
ls .github/actions/setup-protoc
```

Expected: one `uses: ./.github/actions/setup-protoc` hit at line 39; four files listed.

- [ ] **Step 2: Add the general warning above `steps:`**

In `.github/workflows/release-plz.yml`, find:

```yaml
  release-plz:
    name: release-plz
    runs-on: ubuntu-latest
    steps:
```

Replace with:

```yaml
  release-plz:
    name: release-plz
    runs-on: ubuntu-latest
    # NB: this workflow runs only on push to main. A PR never runs it, so a
    # change here is not tested by the PR that makes it.
    steps:
```

- [ ] **Step 3: Remove the protoc step and its comment**

Find and delete exactly these seven lines (nothing before or after them):

```yaml
      # Pinned, checksum-verified protoc — `cargo publish --verify` compiles the
      # workspace (temporalio-protos → prost-wkt-types build.rs needs protoc);
      # without this the runtime-temporal tarball verify fails (seen 2026-07-06).
      # The version and its digests live in the action (SMA-458). NB: this
      # workflow runs only on push to main, so a change here is never exercised
      # by the PR that makes it.
      - uses: ./.github/actions/setup-protoc
```

- [ ] **Step 4: Delete the action**

```bash
git rm -r -q .github/actions/setup-protoc
test ! -e .github/actions/setup-protoc && echo "action deleted"
```

Expected: `action deleted`.

- [ ] **Step 5: Parse and check the workflow**

```bash
ruby -ryaml -e 'y=YAML.load_file(".github/workflows/release-plz.yml"); s=y["jobs"]["release-plz"]["steps"]; puts s.map{|x| x["uses"] || x["name"] || x["run"].to_s[0,40]}; abort("LOCAL ACTION LEFT") if s.any?{|x| x["uses"].to_s.start_with?("./.github/actions/")}; puts "ok"'
git diff -- .github/workflows/release-plz.yml
```

Expected: the step list has no `./.github/actions/` entry and ends with `ok`. The diff shows only the 2 added comment lines and the 7 removed lines.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/release-plz.yml
git commit -m "chore(workflows): SMA-687 drop setup-protoc from release-plz and delete the action

The published baselines (runtime-temporal 0.5.1, facade 0.6.3) enable
vendored-protox, so cargo-semver-checks and cargo publish --verify need no
system protoc. This removes the last protoc install and the PROTOC_VERSION pin.

Co-Authored-By: <model> <noreply@anthropic.com>"
git show --stat HEAD
```

Expected: the commit contains `release-plz.yml` and the four deleted action files, and nothing else.

---

### Task 2: Update `CLAUDE.md` and the CI runbook

**Files:**
- Modify: `CLAUDE.md` (line 112 and line 116)
- Modify: `docs/runbooks/ci-architecture.md` (line 4; lines 107-113; line 121)

**Interfaces:**
- Consumes: Task 1 (the action no longer exists).
- Produces: documents with no instruction to bump or use the protoc pin, except the history and recovery text.

- [ ] **Step 1: Prove the stale text (the "failing test")**

```bash
grep -n 'Four pins\|the protoc bump runbook' CLAUDE.md
grep -n 'until SMA-687\|Nothing tracks the protoc pin\|PROTOC_VERSION' docs/runbooks/ci-architecture.md
```

Expected: hits in both files.

- [ ] **Step 2: Confirm each pin that the new CLAUDE.md sentence names**

```bash
grep -n 'TEMPORAL_CLI_VERSION\|TEMPORAL_CLI_SHA256' .github/workflows/integration.yml | head -2
grep -n 'NIGHTLY_TOOLCHAIN:' .github/workflows/ci.yml
grep -n 'tool: convco@' .github/workflows/ci.yml
grep -n 'MDBOOK_VERSION:\|MDBOOK_LINKCHECK_VERSION:' .github/workflows/docs.yml
grep -n '"markdownlint-cli2"' package-lock.json | head -1
```

Expected: every command prints at least one line. If one prints nothing, STOP and report; do not name that pin.

- [ ] **Step 3: Edit `CLAUDE.md`**

Find:

```markdown
Four pins are hand-bumped with nothing tracking them: `PROTOC_VERSION` and its three digests in `.github/actions/setup-protoc/install.sh` (since SMA-623 used only by `release-plz.yml`, for the semver-check baseline; SMA-687 removes it), `TEMPORAL_CLI_VERSION`/`TEMPORAL_CLI_SHA256` in `integration.yml`, `NIGHTLY_TOOLCHAIN` in `ci.yml`, and `markdownlint-cli2` via `package-lock.json`.
```

Replace with:

```markdown
These pins are hand-bumped with nothing tracking them: `TEMPORAL_CLI_VERSION`/`TEMPORAL_CLI_SHA256` in `integration.yml`, `NIGHTLY_TOOLCHAIN` in `ci.yml`, `convco@…` in `ci.yml`'s `commits` job, `MDBOOK_VERSION`/`MDBOOK_LINKCHECK_VERSION` in `docs.yml`, and `markdownlint-cli2` via `package-lock.json`.
```

Keep the text that follows on the same line (`**A checksum mismatch is never a signal to update the digest** …`) unchanged.

Find:

```markdown
Job-by-job rationale, the protoc bump runbook, the audit/deny signal semantics,
```

Replace with:

```markdown
Job-by-job rationale, the audit/deny signal semantics,
```

- [ ] **Step 4: Runbook scope line**

In `docs/runbooks/ci-architecture.md`, find `(SMA-306/330/335/452/457/458/479/486/487/581/618/623).` and replace with `(SMA-306/330/335/452/457/458/479/486/487/581/618/623/687).`

- [ ] **Step 5: Runbook history paragraph (in place of the paragraph before the recovery paragraph)**

Replace the whole paragraph that starts `**\`release-plz.yml\` still installs \`protoc\`, on purpose, until SMA-687.**` (one line) with:

```markdown
**After SMA-687, no workflow installs `protoc`.** SMA-458 built `.github/actions/setup-protoc`, a repo-local action that installed a pinned, checksum-verified protoc 35.1. SMA-623 made it unnecessary for every job except `release-plz`. `release-plz` kept it because `cargo-semver-checks` builds the published baseline, and `paigasus-helikon-runtime-temporal` 0.5.0 had no `vendored-protox`. SMA-687 deleted the action and the `PROTOC_VERSION` pin after release-plz published `runtime-temporal` 0.5.1 and the facade 0.6.3 with the feature. Commit `0d5e80be0bcc2d5854829831e8ecf1f9c1eaf8ed` contains the action.
```

- [ ] **Step 6: Runbook recovery paragraph**

In the paragraph that starts `**Recovery, if \`protox\` cannot compile a future Temporal proto**`, find:

```markdown
The action is in `.github/actions/setup-protoc/` until SMA-687; after SMA-687, restore it from the git history.
```

Replace with:

```markdown
Restore the action from the git history: `git checkout 0d5e80be0bcc2d5854829831e8ecf1f9c1eaf8ed -- .github/actions/setup-protoc`. Also add the step to `release-plz.yml` again, because without `vendored-protox`, `cargo publish --verify` and the semver check need `protoc`. Do not use `arduino/setup-protoc` instead: its `version` input defaults to `23.x`, not to the latest release. To bump the restored pin, read this section at that commit. If a published baseline fails in `protox` (for example, after a semver-compatible `temporalio-protos` release), restoring `protoc` does not help, because the baseline enables `vendored-protox`. Then set `semver_check = false` for `paigasus-helikon-runtime-temporal` and `paigasus-helikon` in `release-plz.toml` until a release without the feature is the baseline.
```

Keep the last sentence of the paragraph (`Downstream users who hit the same problem can pin …`) after the new text.

- [ ] **Step 7: Delete the two paragraphs after the recovery paragraph**

Delete the paragraph that starts `**\`.github/actions/setup-protoc\`** (SMA-458) is a repo-local composite action.` and the paragraph that starts `**Nothing tracks the protoc pin — bumping it is a human act with no prompt.**`, with their blank separator lines, so that exactly one blank line stays before `## pr-title.yml`.

- [ ] **Step 8: Runbook markdownlint section**

Find:

```markdown
alongside `TEMPORAL_CLI_VERSION` in `integration.yml` and `PROTOC_VERSION` in `.github/actions/setup-protoc/install.sh` (used only by `release-plz.yml` since SMA-623).
```

Replace with:

```markdown
alongside `TEMPORAL_CLI_VERSION` in `integration.yml` and `NIGHTLY_TOOLCHAIN` in `ci.yml`.
```

- [ ] **Step 9: Check the section order and lint**

```bash
awk '/^## protoc and protox/{f=1} /^## pr-title.yml/{f=0} f && /^\*\*/{print NR": "substr($0,1,60)}' docs/runbooks/ci-architecture.md
npx markdownlint-cli2
/opt/homebrew/bin/bash scripts/check-markdownlint-config.sh
```

Expected: exactly four bold-led paragraphs in the section, in this order: `**The workspace compiles its`, `**Two guards in`, `**After SMA-687, no workflow installs`, `**Recovery, if`. Lint: `0 error(s)` / `0 issues`; config check passes.

- [ ] **Step 10: Prove the stale text is gone, then commit**

```bash
grep -n 'Four pins\|the protoc bump runbook' CLAUDE.md; echo "exit $?"
grep -n 'until SMA-687\|Nothing tracks the protoc pin' docs/runbooks/ci-architecture.md; echo "exit $?"
git add CLAUDE.md docs/runbooks/ci-architecture.md
git commit -m "docs(docs): SMA-687 retire the protoc pin from CLAUDE.md and the CI runbook

Co-Authored-By: <model> <noreply@anthropic.com>"
git show --stat HEAD
```

Expected: `exit 1` twice; the commit contains exactly the two files.

---

### Task 3: Full verification (spec section 7, steps 1-6)

**Files:** none expected to change.

**Interfaces:**
- Consumes: Tasks 1 and 2.
- Produces: a verification report.

- [ ] **Step 1:** `git grep -nE 'setup-protoc|PROTOC_VERSION' -- ':!docs/superpowers'` → the only hits are in `docs/runbooks/ci-architecture.md`: the history paragraph and the recovery paragraph. List every hit.
- [ ] **Step 2:** `test ! -e .github/actions && echo "no local actions"` → prints `no local actions`.
- [ ] **Step 3:** `for f in .github/workflows/*.yml; do ruby -ryaml -e 'YAML.load_file(ARGV[0])' "$f" || echo "PARSE FAIL $f"; done; echo done` → no `PARSE FAIL`.
- [ ] **Step 4:** `npx markdownlint-cli2` → 0 errors; `/opt/homebrew/bin/bash scripts/check-markdownlint-config.sh` → passes.
- [ ] **Step 5:** `/opt/homebrew/bin/bash scripts/check-cargo-profile-env-sync.sh` and `/opt/homebrew/bin/bash scripts/check-cargo-profile-env-sync-selftest.sh` → both pass.
- [ ] **Step 6:** `git diff --name-status 0d5e80be0bcc2d5854829831e8ecf1f9c1eaf8ed HEAD` → only: `.github/workflows/release-plz.yml` (M), the four `.github/actions/setup-protoc/*` files (D), `CLAUDE.md` (M), `docs/runbooks/ci-architecture.md` (M), the spec and this plan (A).

Report each step with its command and output. Do not claim a pass without output.

---

## After the plan (not tasks for implementers)

Stage 5 local review; Stage 6 PR titled `chore(workflows): SMA-687 retire setup-protoc and the protoc pin`; spec section 7 steps 7-10 (CI incl. `sessions-it`/`temporal-it`, post-merge `release-plz` log, close-out comment on SMA-687 with E5/E6 and the live-confirmation note).
