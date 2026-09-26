# SMA-687 — Retire `setup-protoc` and the protoc pin (SMA-623 phase 2)

- **Ticket:** [SMA-687](https://linear.app/smaschek/issue/SMA-687)
- **Date:** 2026-09-26
- **Status:** Design approved in conversation. Revised after the adversarial
  spec challenge (section 10). This spec is for Gate 1 review.
- **Parent design:** `docs/superpowers/specs/2026-09-25-sma-623-vendored-protox-design.md`, section 5.

## 1. Problem

SMA-623 (PR #269) enabled `temporalio-client`'s `vendored-protox` feature, so the
workspace compiles the Temporal protos with `protox` and needs no system
`protoc`. It removed `setup-protoc` from 8 of 9 CI sites. It kept the step in
`.github/workflows/release-plz.yml` on purpose: release-plz runs
`cargo-semver-checks` by default, and that check builds the **published registry
baseline**. The baseline was `paigasus-helikon-runtime-temporal` 0.5.0 and the
facade 0.6.2, which have no `vendored-protox`, so their build called `protoc`.

Thus the repo still has `.github/actions/setup-protoc/` and the hand-bumped pin
`PROTOC_VERSION` with its three SHA-256 digests.

## 2. The trigger is met

SMA-687 must start only after a published `runtime-temporal` version and a
published facade version include `vendored-protox`. Both conditions are true:

- release-plz run `36197350661` (on `0d5e80be`, the merge of release PR #270)
  succeeded and published both versions.
- The **published** manifest of `paigasus-helikon-runtime-temporal` 0.5.1
  (downloaded from `static.crates.io`) declares
  `[dependencies.temporalio-client]` with
  `features = ["tls-aws-lc", "vendored-protox"]`.
- The published manifest of `paigasus-helikon` 0.6.3 declares
  `[dependencies.paigasus-helikon-runtime-temporal]` with `version = "0.5.1"`.

## 3. Evidence that the new baseline needs no `protoc`

All runs used `PROTOC=/nonexistent/protoc` on the development host, which has a
Homebrew `protoc` on `PATH` (prost-build reads `PROTOC` before `PATH`).

| # | Check | Result |
|---|---|---|
| E1 | Scratch project with `paigasus-helikon = "=0.6.3"` (feature `runtime-temporal`) and `paigasus-helikon-runtime-temporal = "=0.5.1"`: `cargo build` | exit 0 |
| E2 | Same project: `cargo doc --no-deps` for each crate | exit 0 |
| E3 | Same project: `cargo tree -p paigasus-helikon-runtime-temporal -e features -i prost-wkt-types` | shows `prost-wkt-types feature "vendored-protox"` |
| E4 | Control: the same project with `=0.6.2` / `=0.5.0` | exit 101, "Could not find `protoc`" in `temporalio-protos` and `prost-wkt-types` |
| **E5** | **`cargo-semver-checks` 0.50.0** from the worktree: `cargo semver-checks check-release -p paigasus-helikon-runtime-temporal` (baseline = registry 0.5.1, built: 31 s) | exit 0, "196 checks: 196 pass", "no semver update required" |
| **E6** | `cargo semver-checks check-release -p paigasus-helikon` (baseline = registry 0.6.3, default feature heuristic, built: 41 s) and again with `--all-features` | exit 0 both, "no semver update required" |
| E7 | Control: `cargo semver-checks check-release -p paigasus-helikon-runtime-temporal --baseline-version 0.5.0` | exit 101, "Could not find `protoc`" while it builds the 0.5.0 baseline |

E5 and E6 run the same tool that release-plz runs, with its own baseline
selection and feature heuristic. E4 and E7 prove that the probe detects a
missing `protoc`. The `--all-features` run in E6 reused the cached baseline
rustdoc from the default-feature run; the parent spec (section 2) found only two
protoc-calling build scripts in the complete all-features graph, and both use
`protox` in the baseline (E3).

**The first release-plz run after this merge does not test this.** release-plz
runs `cargo-semver-checks` only for a package that changed. This PR changes no
crate file (section 5.5), so that run semver-checks nothing. E5 and E6 are the
proof. The first live confirmation is the first later release run that
semver-checks `paigasus-helikon-runtime-temporal` or the facade; section 7,
step 10 names who watches it.

## 4. Decision

Remove the last `protoc` install now. Delete the action and the pin.

Rejected: a `PROTOC` guard in `release-plz.yml`. `release-plz` is the publish
path. A strict `PROTOC` there adds a way for the publish to fail and adds no
safety. (The `ci.yml` guards check the locked workspace graph, not the
published baseline, so they are not the reason.)

## 5. Changes

### 5.1 `.github/workflows/release-plz.yml`

- Delete lines 33-39: the comment that starts
  `# Pinned, checksum-verified protoc — \`cargo publish --verify\` compiles the`
  and the step `- uses: ./.github/actions/setup-protoc`.
- Keep the warning that the old comment carried, as a general comment directly
  above the job's `steps:` key:
  `# NB: this workflow runs only on push to main. A PR never runs it, so a change here is not tested by the PR that makes it.`
  (Split it over two comment lines if the line is too long for the file's
  style.)
- Change nothing else in the file.

### 5.2 `.github/actions/setup-protoc/`

Delete the directory with its four files: `action.yml`, `install.sh`,
`verify.sh`, `selftest.sh`. It is the only directory in `.github/actions/`, so
`.github/actions/` disappears too.

The `'.github/actions/**'` entries in the path filters of `sessions-it`
(`ci.yml`) and `temporal-it` (`integration.yml`) **stay**. Their comments are
already generic (SMA-623). A glob that matches no file costs nothing.

### 5.3 `CLAUDE.md`

- Replace the sentence that starts `Four pins are hand-bumped with nothing
  tracking them:` up to `via \`package-lock.json\`.` with:
  `These pins are hand-bumped with nothing tracking them: \`TEMPORAL_CLI_VERSION\`/\`TEMPORAL_CLI_SHA256\` in \`integration.yml\`, \`NIGHTLY_TOOLCHAIN\` in \`ci.yml\`, \`convco@…\` in \`ci.yml\`'s \`commits\` job, \`MDBOOK_VERSION\`/\`MDBOOK_LINKCHECK_VERSION\` in \`docs.yml\`, and \`markdownlint-cli2\` via \`package-lock.json\`.`
  (Dependabot follows only the action SHAs, not the tool versions that
  `taiki-e/install-action` receives. The implementation must confirm each named
  pin exists with a `grep` before it writes the sentence.)
- Keep the next sentence (`**A checksum mismatch is never a signal to update the
  digest** …`) unchanged. `TEMPORAL_CLI_SHA256` still uses it.
- In the sentence `Job-by-job rationale, the protoc bump runbook, the audit/deny
  signal semantics, …`, remove `the protoc bump runbook, `.
- Keep the paragraph `**The workspace needs no system \`protoc\`** (SMA-623): …`
  unchanged.

### 5.4 `docs/runbooks/ci-architecture.md`

- Scope line: change `…/581/618/623).` to `…/581/618/623/687).`
- In section "protoc and protox", keep the first two paragraphs (the `protox`
  mechanism and the two guards) unchanged.
- **In place of** the paragraph that starts
  `**\`release-plz.yml\` still installs \`protoc\`, on purpose, until SMA-687.**`
  (it is directly before the recovery paragraph), put this history paragraph:

  > **After SMA-687, no workflow installs `protoc`.** SMA-458 built `.github/actions/setup-protoc`, a repo-local action that installed a pinned, checksum-verified protoc 35.1. SMA-623 made it unnecessary for every job except `release-plz`. `release-plz` kept it because `cargo-semver-checks` builds the published baseline, and `paigasus-helikon-runtime-temporal` 0.5.0 had no `vendored-protox`. SMA-687 deleted the action and the `PROTOC_VERSION` pin after release-plz published `runtime-temporal` 0.5.1 and the facade 0.6.3 with the feature. Commit `0d5e80be0bcc2d5854829831e8ecf1f9c1eaf8ed` contains the action.

- Delete the two paragraphs **after** the recovery paragraph: the one that
  starts `**\`.github/actions/setup-protoc\`** (SMA-458)` and the one that starts
  `**Nothing tracks the protoc pin`.
- In the recovery paragraph (starts `**Recovery, if \`protox\` cannot compile a
  future Temporal proto**`), replace
  `The action is in \`.github/actions/setup-protoc/\` until SMA-687; after SMA-687, restore it from the git history.`
  with these sentences:
  `Restore the action from the git history: \`git checkout 0d5e80be0bcc2d5854829831e8ecf1f9c1eaf8ed -- .github/actions/setup-protoc\`. Also add the step to \`release-plz.yml\` again, because without \`vendored-protox\`, \`cargo publish --verify\` and the semver check need \`protoc\`. Do not use \`arduino/setup-protoc\` instead: its \`version\` input defaults to \`23.x\`, not to the latest release. To bump the restored pin, read this section at that commit. If a published baseline fails in \`protox\` (for example, after a semver-compatible \`temporalio-protos\` release), restoring \`protoc\` does not help, because the baseline enables \`vendored-protox\`. Then set \`semver_check = false\` for \`paigasus-helikon-runtime-temporal\` and \`paigasus-helikon\` in \`release-plz.toml\` until a release without the feature is the baseline.`
- In section "markdownlint pinning", replace
  `alongside \`TEMPORAL_CLI_VERSION\` in \`integration.yml\` and \`PROTOC_VERSION\` in \`.github/actions/setup-protoc/install.sh\` (used only by \`release-plz.yml\` since SMA-623).`
  with
  `alongside \`TEMPORAL_CLI_VERSION\` in \`integration.yml\` and \`NIGHTLY_TOOLCHAIN\` in \`ci.yml\`.`

### 5.5 Not changed

- `CONTRIBUTING.md`: it has no `setup-protoc` or `PROTOC_VERSION` mention since
  SMA-623.
- `ci.yml`, `integration.yml`, `msrv.yml`, `Cargo.toml`, `Cargo.lock`, any crate
  file, `docs/book/`, crate READMEs: no change. This is a pure CI and
  documentation change, so the mdBook and the READMEs need no edit (a conscious
  decision).
- Earlier specs and plans under `docs/superpowers/`: historical records.

## 6. Version and release

- release-plz attributes a release by crate path. This PR touches no crate path,
  so it releases nothing and bumps no version.
- Release PR #271 (`chore: release`, `paigasus-helikon-cli` 0.1.30 → 0.1.31) is
  open. After this merge, release-plz regenerates #271. That is expected and
  unrelated.
- PR title: `chore(workflows): SMA-687 retire setup-protoc and the protoc pin`.
  `CLAUDE.md` requires `chore(...)` or `docs(...)` for `release-plz.yml` edits.
  `chore` and `workflows` are both on the `pr-title.yml` allowlist.

## 7. Verification

Before the push:

1. `git grep -nE 'setup-protoc|PROTOC_VERSION' -- ':!docs/superpowers'` finds
   exactly the history paragraph and the recovery sentences in
   `docs/runbooks/ci-architecture.md`, and nothing else.
2. `test ! -e .github/actions/setup-protoc`.
3. Every workflow file parses (`ruby -ryaml`), and `release-plz.yml` has no step
   that `uses: ./.github/actions/`.
4. `npx markdownlint-cli2` and `scripts/check-markdownlint-config.sh` pass.
5. `scripts/check-cargo-profile-env-sync.sh` and its self-test pass (no env
   changed, but the script reads every workflow).
6. The PR diff touches only: `release-plz.yml`, the four action files,
   `CLAUDE.md`, `docs/runbooks/ci-architecture.md`, this spec, and the plan.

On the PR:

7. All 15 required contexts report and are green. The deleted files match the
   `'.github/actions/**'` filters, so `sessions-it` (required) and `temporal-it`
   (signal-only) run their full suites. For `temporal-it`, a Temporal server
   timeout is the known flake: one rerun is acceptable.

After the merge:

8. The `release-plz` run on the merge commit passes. Read its log: it must show
   no `setup-protoc` step. It semver-checks nothing (section 3).
9. A close-out comment on SMA-687 with the run id and the E5/E6 result.
10. **Live confirmation:** the first later release run that semver-checks
    `paigasus-helikon-runtime-temporal` or the facade. The close-out comment on
    SMA-687 names this and asks the person who merges that release to read the
    `release-pr` log for "Could not find `protoc`". If it fails, section 8
    gives the rollback.

## 8. Risks

| Risk | Effect | Mitigation |
|---|---|---|
| `cargo-semver-checks` in CI differs from E5/E6 (for example, a newer tool version or a different feature selection) and needs `protoc`. | A later release run fails. | E5/E6 used the same tool and its defaults. Rollback: `git revert` the SMA-687 merge commit. That restores the step, the action and the documents together. |
| A later Temporal release breaks `protox`. | Covered by the SMA-623 recovery path, now with an exact commit to restore from. | Runbook recovery paragraph. |
| A published baseline itself fails in `protox`. | Release runs fail; restoring `protoc` does not help. | Runbook: set `semver_check = false` for the two crates in `release-plz.toml`. |

## 9. Acceptance (from the ticket)

- `grep` for `setup-protoc` and `PROTOC_VERSION` finds nothing except the
  history paragraph and the recovery sentences in the runbook: step 1.
- The first `release-plz` run after the merge passes: step 8. No release is
  expected from this PR, because no crate changes.

## 10. Spec challenge changelog

The Opus spec-challenger returned **APPROVE WITH CHANGES** (0 BLOCKER, 1 MAJOR,
12 MINOR, 4 QUESTION). All findings were folded in; none was rejected.

- **MAJOR — the post-merge run does not semver-check anything.** Accepted. Added
  E5-E7 with the real `cargo-semver-checks` 0.50.0 (installed only in the
  scratchpad), and step 10 for the live confirmation.
- MINOR — keep the "release-plz.yml never runs on a PR" warning: section 5.1.
- MINOR — "Three pins" was a wrong count: section 5.3 lists the pins without a
  number and adds `convco`, `MDBOOK_VERSION` and `MDBOOK_LINKCHECK_VERSION`.
- MINOR — "last commit" claim and short SHA: the full SHA, and "contains".
- MINOR — rollback by `git revert`: risk row 1.
- MINOR — the reason for rejecting a `release-plz` guard: section 4.
- MINOR — the case where the baseline itself breaks: recovery paragraph and
  risk row 3.
- MINOR — E2/E3 were not the same rustdoc as `cargo-semver-checks`: replaced by
  E5/E6.
- MINOR — the `.versionrc` reasoning and the commit type: section 6 now uses the
  path argument and `chore(workflows)`.
- MINOR — `sessions-it` and `temporal-it` will run: step 7.
- MINOR — the grep scope and the expected hits: step 1 and section 9.
- MINOR — where the history paragraph goes: section 5.4.
- MINOR — the `arduino/setup-protoc` trap and the bump pointer: recovery
  paragraph.
- MINOR — STE wording: fixed in the new text.
- QUESTIONS: how release-plz handles a failed baseline build is now moot (E5/E6
  pass). Whether `ubuntu-latest` ships a `protoc` does not affect E5-E7, which
  point `PROTOC` at a path that does not exist. The owner of the live
  confirmation is in step 10. A release PR (#271) is open; see section 6.
