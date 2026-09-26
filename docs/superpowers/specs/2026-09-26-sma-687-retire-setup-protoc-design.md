# SMA-687 — Retire `setup-protoc` and the protoc pin (SMA-623 phase 2)

- **Ticket:** [SMA-687](https://linear.app/smaschek/issue/SMA-687)
- **Date:** 2026-09-26
- **Status:** Design approved in conversation. This spec is for Gate 1 review.
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
`PROTOC_VERSION` with its three SHA-256 digests. `CLAUDE.md` lists it as one of
four untracked pins.

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

A throwaway project in the scratchpad depended on exactly the new baseline:
`paigasus-helikon = { version = "=0.6.3", features = ["runtime-temporal"] }` and
`paigasus-helikon-runtime-temporal = "=0.5.1"`. All runs used
`PROTOC=/nonexistent/protoc`.

| # | Check | Result |
|---|---|---|
| E1 | `cargo build` | exit 0 |
| E2 | `cargo doc -p paigasus-helikon-runtime-temporal --no-deps` | exit 0 |
| E3 | `cargo doc -p paigasus-helikon --no-deps` | exit 0 |
| E4 | `cargo tree -p paigasus-helikon-runtime-temporal -e features -i prost-wkt-types` | shows `prost-wkt-types feature "vendored-protox"` |
| E5 | Control: the same project with `=0.6.2` / `=0.5.0` | exit 101, "Could not find `protoc`" in `temporalio-protos` and `prost-wkt-types` |

E2 and E3 build the same rustdoc that `cargo-semver-checks` builds for the
baseline. E5 proves that the probe detects a missing `protoc`. The new tarballs
that `cargo publish --verify` builds use the workspace manifest, which has had
the feature since SMA-623.

What E1-E5 do not prove: that `cargo-semver-checks` itself calls nothing else
that needs `protoc`. It builds rustdoc JSON of the baseline with cargo, which is
the same compile as E2/E3. The first `release-plz` run after the merge is the
final proof (section 7).

## 4. Decision

Remove the last `protoc` install now. Delete the action and the pin.

Rejected: a `PROTOC` guard in `release-plz.yml`. Guard 1 and Guard 2 in
`ci.yml` already run on every PR and block a regression before it reaches
`main`. A guard in `release-plz.yml` fires only after a merge.

## 5. Changes

### 5.1 `.github/workflows/release-plz.yml`

Delete lines 33-39: the comment that starts
`# Pinned, checksum-verified protoc — \`cargo publish --verify\` compiles the`
and the step `- uses: ./.github/actions/setup-protoc`. Change nothing else in
the file.

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
  `Three pins are hand-bumped with nothing tracking them: \`TEMPORAL_CLI_VERSION\`/\`TEMPORAL_CLI_SHA256\` in \`integration.yml\`, \`NIGHTLY_TOOLCHAIN\` in \`ci.yml\`, and \`markdownlint-cli2\` via \`package-lock.json\`.`
  Keep the next sentence (`**A checksum mismatch is never a signal to update the
  digest** …`) unchanged. `TEMPORAL_CLI_SHA256` still uses it.
- In the sentence `Job-by-job rationale, the protoc bump runbook, the audit/deny
  signal semantics, …`, remove `the protoc bump runbook, `.
- Keep the paragraph `**The workspace needs no system \`protoc\`** (SMA-623): …`
  unchanged.

### 5.4 `docs/runbooks/ci-architecture.md`

- Scope line: change `…/581/618/623).` to `…/581/618/623/687).`
- In section "protoc and protox", keep the first two paragraphs (the `protox`
  mechanism and the two guards) unchanged.
- Replace the three paragraphs that start
  `**\`release-plz.yml\` still installs \`protoc\`, on purpose, until SMA-687.**`,
  `**\`.github/actions/setup-protoc\`** (SMA-458)` and
  `**Nothing tracks the protoc pin` with **one** history paragraph:

  > **No workflow installs `protoc` since SMA-687.** SMA-458 built `.github/actions/setup-protoc`, a repo-local action that installed a pinned, checksum-verified protoc 35.1. SMA-623 made it unnecessary for every job except `release-plz`. `release-plz` kept it because `cargo-semver-checks` builds the published baseline, and `paigasus-helikon-runtime-temporal` 0.5.0 had no `vendored-protox`. SMA-687 deleted the action and the `PROTOC_VERSION` pin after `runtime-temporal` 0.5.1 and the facade 0.6.3 were published with the feature. The last commit that contains the action is `0d5e80be`.

- In the recovery paragraph (starts `**Recovery, if \`protox\` cannot compile a
  future Temporal proto**`), replace
  `The action is in \`.github/actions/setup-protoc/\` until SMA-687; after SMA-687, restore it from the git history.`
  with
  `Restore the action from the git history: \`git checkout 0d5e80be -- .github/actions/setup-protoc\`. Also add the step to \`release-plz.yml\` again, because \`cargo publish --verify\` and the semver check then need \`protoc\`.`
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

- No crate path changes, so release-plz has nothing to release. After the merge,
  release-plz is expected to run, publish nothing, and open or update no release
  PR. That is the correct result.
- PR title: `ci(workflows): SMA-687 retire setup-protoc and the protoc pin`.
  `CLAUDE.md` requires `chore(...)`/`docs(...)`-class types, never `feat`/`fix`,
  for `release-plz.yml` edits. `ci` has `increment: None` in `.versionrc`, and
  release-plz attributes bumps by crate path, so no crate is bumped.

## 7. Verification

Before the push:

1. `grep -rnE 'setup-protoc|PROTOC_VERSION' .github CLAUDE.md CONTRIBUTING.md docs/runbooks README.md crates/*/README.md`
   finds only the history paragraph and the recovery sentence in
   `docs/runbooks/ci-architecture.md`.
2. `test ! -e .github/actions/setup-protoc`.
3. Every workflow file parses (`ruby -ryaml`), and `release-plz.yml` has no step
   that `uses: ./.github/actions/`.
4. `npx markdownlint-cli2` and `scripts/check-markdownlint-config.sh` pass.
5. `scripts/check-cargo-profile-env-sync.sh` and its self-test pass (no env
   changed, but the script reads every workflow).
6. The PR diff touches only: `release-plz.yml`, the four action files,
   `CLAUDE.md`, `docs/runbooks/ci-architecture.md`, this spec, and the plan.

On the PR:

7. All 15 required contexts report and are green. No CI job used the action any
   more, so no job result should change.

After the merge:

8. The `release-plz` run on the merge commit passes. Read its log: it must show
   no `setup-protoc` step, and no "Could not find `protoc`" error.
9. Close-out note on SMA-687 with the run id.

## 8. Risks

| Risk | Effect | Mitigation |
|---|---|---|
| `cargo-semver-checks` needs `protoc` for a reason E1-E5 do not cover. | The first `release-plz` run after the merge fails. | E2/E3 build the same rustdoc. If the run fails, read the log. Recovery: `git checkout 0d5e80be -- .github/actions/setup-protoc` and add the step again (runbook). The failure blocks only releases, not `main`. |
| A later Temporal release breaks `protox`. | Covered by the SMA-623 recovery path, now with an exact commit to restore from. | Runbook recovery paragraph. |

## 9. Acceptance (from the ticket)

- `grep -rn "setup-protoc\|PROTOC_VERSION" .github CLAUDE.md CONTRIBUTING.md docs/runbooks`
  finds nothing except the history note: step 1.
- The first `release-plz` run after the merge passes: step 8. No release PR is
  expected, because no crate changes.
