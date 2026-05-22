# SMA-350 — Unblock release-plz for paigasus-helikon-macros + migrate app-token to client-id — design

- **Linear:** [SMA-350](https://linear.app/smaschek/issue/SMA-350/unblock-release-plz-for-paigasus-helikon-macros-migrate-app-token-to)
- **Branch:** `feature/sma-350-unblock-release-plz-for-paigasus-helikon-macros-migrate-app`
- **Status:** design (awaiting implementation plan)
- **Author:** Sven Maschek
- **Date:** 2026-05-22
- **Related:** [SMA-347](https://linear.app/smaschek/issue/SMA-347) (original 0.0.0-trap escape for core + facade), [SMA-307](https://linear.app/smaschek/issue/SMA-307) (initial release-plz setup), [SMA-315](https://linear.app/smaschek/issue/SMA-315) (the merged feature that surfaced both bugs)

## 1. Goal & non-goals

**Goal.** Make the next push to `main` produce a usable release-plz PR, and silence the deprecation warning on the GitHub App token step.

Two unrelated bugs, both surfaced by the post-SMA-315 release-plz run:

1. **The macros 0.0.0 trap.** `paigasus-helikon-macros` is still at `version = "0.0.0"`. release-plz interprets the matching `v0.0.0` tag as "already published" and emits `INFO paigasus-helikon-macros 0.0.0: Already published — Tag paigasus-helikon-macros-v0.0.0 already exists`. The crate now has a real public API (SMA-315) so it must escape the trap.
2. **Deprecated `app-id` input.** `actions/create-github-app-token@v3` warns `Input 'app-id' has been deprecated with message: Use 'client-id' instead.` on both job steps. v4 is expected to remove the input.

Both fixes ride in one PR.

**Non-goals.**

- Investigating why `release-plz-pr` returned `{"prs":[]}` for the whole workspace and not just macros — i.e., why `paigasus-helikon-core` and `paigasus-helikon` (both at `0.1.0`, both touched by `9cd4813`) were not also proposed for a minor bump. The Linear ticket flags this as "likely moot once the macros trap is escaped; if it persists, a separate follow-up to inspect release-plz's pre-1.0 commit-attribution heuristics may be warranted." Out of scope here.
- Bumping any other stub crate (`evals`, `mcp`, `providers-*`, `runtime-*`, `tools`). Per SMA-347 they stay at `0.0.0` until each ships its first real API.
- Touching `RELEASE_PLZ_APP_PRIVATE_KEY` or the paigasusbot App installation itself.
- Migrating to `actions/create-github-app-token@v4` proactively. v3 with `client-id` is the supported path today; v4 bump is a separate Dependabot-driven chore.

## 2. File layout

```text
crates/paigasus-helikon-macros/Cargo.toml           (modified — version "0.0.0" → "0.1.0")
Cargo.toml                                          (modified — [workspace.dependencies]
                                                     paigasus-helikon-macros version
                                                     "0.0.0" → "0.1.0")
.github/workflows/release-plz.yml                   (modified — both `app-id` references
                                                     swap to `client-id`)
docs/superpowers/specs/2026-05-22-sma-350-release-plz-unblock-design.md  (new — this spec)
```

No changes to any crate's `src/`, to `release-plz.toml`, to other workflows (`ci.yml`, `msrv.yml`, `audit.yml`, `deny.yml`, `sbom.yml`), to `deny.toml`, to `CLAUDE.md`, or to `CONTRIBUTING.md`. No new tests.

## 3. Decisions and rationale

| Decision | Choice | Rationale |
|---|---|---|
| Single PR vs two | **One PR covering both fixes.** | Each fix is two lines. The Linear ticket is a single SMA-350. Splitting would double PR-title / CI / review overhead with no benefit. |
| Commit shape inside the PR | **Two commits on the feature branch** (`chore(release): SMA-350 …` for the macros bump, `ci(release-plz): SMA-350 …` for the workflow). Squash-merge collapses them to one `chore(release): SMA-350 …` commit on `main`. | Readable history if anyone walks the feature branch commit-by-commit. Both prefixes are non-`feat`/non-`fix`, consistent with the CLAUDE.md "bootstrap commits on release infrastructure must use `chore(...)` or `docs(...)`" rule — a `feat` would mis-attribute a bump to the whole workspace. |
| Secret strategy for the App credential | **Additive: new `RELEASE_PLZ_APP_CLIENT_ID` secret holding the Client ID (`Iv23li…`).** `RELEASE_PLZ_APP_ID` and `RELEASE_PLZ_APP_PRIVATE_KEY` are left in place. | Safer than rotating `RELEASE_PLZ_APP_ID` in place: zero-downtime, trivial revert (the old secret and the old workflow field both still exist on the rollback path). The Linear ticket called the rotate-in-place option "simpler" but accepted either; we choose safer. The old secret is deleted as a follow-up after one successful post-merge release-plz run. |
| Secret-creation timing | **User creates the secret before merging the PR.** | The release-plz workflow only triggers on `push: branches: [main]`, so the first execution of the new workflow file *is* the post-merge run. If the secret doesn't exist at that moment, the `actions/create-github-app-token` step fails and the release pipeline is broken on `main` until it's added. Creating it pre-merge is the only way to avoid that window. |
| Whether to add `workflow_dispatch` for safer testing | **No.** | Scope creep. The release-plz workflow is intentionally `push: [main]`-only by SMA-307's design (a manual-dispatch surface invites accidental tag pushes). If verification turns out to be flaky, that's a separate ticket. |
| Macros version target | **`0.1.0`**, matching SMA-347's choice for core + facade. | Consistency: every Stage-1 crate that's escaped the 0.0.0 trap is on `0.1.0`. release-plz's next run can then propose bumps for all three (macros, core, facade) in one release PR — assuming the workspace-wide `{"prs":[]}` symptom was downstream of the macros trap, which the Linear ticket judges likely. |
| Workspace-dependencies pin update | **Required, in the same commit as the crate-level bump.** Update `Cargo.toml:42` from `version = "0.0.0"` to `"0.1.0"`. | The `version` next to `path` in `[workspace.dependencies]` is a semver constraint cargo enforces against the path target. For `0.x.y` versions cargo's caret rule is effectively exact (`^0.0.0 ≡ =0.0.0`, `^0.1.0` matches `0.1.*`), so the pin and the crate's declared version must move together — leaving the pin at `0.0.0` while the crate is at `0.1.0` would fail resolution. The SMA-347 precedent already encodes this lockstep update. |

## 4. Detailed diffs

### 4.1 `crates/paigasus-helikon-macros/Cargo.toml`

```diff
 [package]
 name        = "paigasus-helikon-macros"
 description = "Proc macros for the Paigasus Helikon AI SDK."
-version                = "0.0.0"
+version                = "0.1.0"
 edition.workspace      = true
```

### 4.2 `Cargo.toml` (workspace root)

```diff
 paigasus-helikon-core                = { path = "crates/paigasus-helikon-core",                version = "0.1.0" }
-paigasus-helikon-macros              = { path = "crates/paigasus-helikon-macros",              version = "0.0.0" }
+paigasus-helikon-macros              = { path = "crates/paigasus-helikon-macros",              version = "0.1.0" }
 paigasus-helikon-providers-openai    = { path = "crates/paigasus-helikon-providers-openai",    version = "0.0.0" }
```

### 4.3 `.github/workflows/release-plz.yml`

Both occurrences — the `release-plz-release` job (lines 27–31) and the `release-plz-pr` job (lines 49–53):

```diff
       - uses: actions/create-github-app-token@v3
         id: app-token
         with:
-          app-id: ${{ secrets.RELEASE_PLZ_APP_ID }}
+          client-id: ${{ secrets.RELEASE_PLZ_APP_CLIENT_ID }}
           private-key: ${{ secrets.RELEASE_PLZ_APP_PRIVATE_KEY }}
```

`RELEASE_PLZ_APP_PRIVATE_KEY` is unchanged.

## 5. Out-of-repo prerequisite (operator action)

Before the PR merges, the user adds one new GitHub repo secret:

- **Name:** `RELEASE_PLZ_APP_CLIENT_ID`
- **Value:** the paigasusbot GitHub App's Client ID — a string with the form `Iv23li…`. Source: GitHub → Settings → Developer settings → GitHub Apps → paigasusbot → "About" section → "Client ID" field (distinct from the numeric App ID `3742291`).
- **Scope:** the `SMK1085/paigasus-helikon` repo's Actions secrets (Settings → Secrets and variables → Actions → New repository secret).

`RELEASE_PLZ_APP_ID` and `RELEASE_PLZ_APP_PRIVATE_KEY` are left in place untouched. After one successful post-merge release-plz run on the new path, `RELEASE_PLZ_APP_ID` can be deleted as a janitorial follow-up.

## 6. Verification

**Pre-merge — local + CI.**

- `cargo build --workspace --all-features` confirms the macros version bump compiles cleanly (it's a pure manifest change with no path-version-pinned external consumer beyond the workspace itself).
- The standard PR gates fire: `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`. The two-commit feature-branch shape passes `commits` (both are Conventional Commits); the squashed PR title `chore(release): SMA-350 unblock release-plz for macros and migrate to client-id` passes `pr-title` (starts with lowercase `u` after `SMA-350 `).

**Post-merge — release-plz workflow run on `main`.**

The Actions tab (https://github.com/SMK1085/paigasus-helikon/actions/workflows/release-plz.yml) is the canonical verification surface:

- [ ] No `Input 'app-id' has been deprecated` annotation on either `release-plz-release` or `release-plz-pr`.
- [ ] `actions/create-github-app-token` step succeeds — token is minted, no 401/credential-error.
- [ ] `release-plz-pr` opens a `chore: release` PR including at minimum a version bump for `paigasus-helikon-macros`, and very likely bumps for `paigasus-helikon-core` and `paigasus-helikon` as well — picking up the SMA-315 changes that the previous run dropped on the floor. The exact bump magnitudes (patch vs minor) are release-plz's call from the Conventional Commit history; the spec deliberately doesn't pin them.

If the release PR still excludes core + facade despite the macros trap being escaped, that's the "out of scope" investigation the Linear ticket flagged.

## 7. Error handling & rollback

- **Secret missing or wrong value at merge time.** `actions/create-github-app-token` fails with a credential error. Fix: correct the secret value, re-run the workflow from the Actions UI. No code revert is needed because the workflow file itself is correct.
- **App-token action somehow rejects `client-id`** (e.g. an upstream regression). Revert the workflow commit. `RELEASE_PLZ_APP_ID` is still in place (additive-secret strategy paid for itself); the deprecation warning returns but the workflow runs.
- **Macros bump breaks the workspace build.** Extremely unlikely — pure version-string change with no behavioral effect — but if it does, revert the macros-bump commit. Nothing downstream pin-locks the macros version above the workspace pin yet.
- **The next release-plz run still returns `{"prs":[]}`.** Not a regression of this PR; falls into the SMA-350 "out of scope" follow-up bucket. The deprecation warning is gone and the macros trap is escaped regardless.

## 8. Acceptance criteria

Mirrors the Linear ticket exactly:

- [ ] `paigasus-helikon-macros` is at `0.1.0` in both `crates/paigasus-helikon-macros/Cargo.toml` and the workspace `[workspace.dependencies]` table.
- [ ] After this PR merges to `main`, release-plz opens a `chore: release` PR that includes a version bump for at least `paigasus-helikon-macros`.
- [ ] The post-merge release-plz workflow run shows no `app-id … deprecated` annotation.
- [ ] No regression: `commit-msg` hook, branch protection, and the `pr-title` gate all stay green on this PR and on the release PR that follows.
