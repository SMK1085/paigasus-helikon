# SMA-385 — Lift workspace `publish = false` to unblock release-plz

**Linear issue**: [SMA-385](https://linear.app/smaschek/issue/SMA-385/release-plz-isnt-bumping-paigasus-helikon-core-past-010-on-featcore)
**Status**: design approved 2026-05-28
**Branch**: `feature/sma-385-release-plz-isnt-bumping-paigasus-helikon-core-past-010-on`
**Depends on**: SMA-307 (release-plz scaffold — landed)
**Related**: SMA-347, SMA-350, SMA-372, SMA-382 (the four 0.0.0-trap manual escapes — same root cause, different symptom)

## 1. Goal & non-goals

**Goal.** Get release-plz proposing version bumps automatically when `feat(*)` / `fix(*)` commits land on `main`. The ticket symptom is that `paigasus-helikon-core` stayed at `0.1.0` across two `feat(core)` merges (SMA-318 MemorySession, SMA-319 typestate builder), but the same bug affects every released crate in the workspace.

The fix is to move from the unsupported `publish = false` mode to the supported "publish to crates.io" mode (Stage 1 of the original SMA-307 plan). After this lands:

1. The six crates with real public API (`-core`, the facade, `-macros`, `-providers-openai`, `-providers-anthropic`, `-sessions-sqlite`) are published to crates.io at `0.1.0`.
2. The seven docstring-only stubs (`-mcp`, `-tools`, `-evals`, `-runtime-{tokio,axum,temporal,agentcore}`) are pinned with per-package `publish = false` *and* `release = false` so cargo refuses to publish them and release-plz ignores them entirely.
3. `paigasus-helikon-cli` retains its existing `publish = false` (binary-only, never published as a library).
4. CLAUDE.md is updated to reflect that Stage 1 has happened, and the 0.0.0-trap-escape recipe gains a fourth step covering the new per-package toggles.

**Non-goals.**

- Filing the upstream release-plz bug (canonicalize-on-missing-`Cargo.lock` in the no-registry fallback path). A clean repro exists locally; a separate ticket can carry it upstream if we want to be a good citizen.
- Lifting `publish = false` on individual stubs as they ship real API. That stays a per-ticket concern, handled by the new variant of the 0.0.0-trap-escape pattern (see §6).
- Yanking any prior crates.io entries. None exist — the workspace has never published anything; the `paigasus-helikon-*-v0.0.0` and `-v0.1.0` git tags are local-only.
- Switching to a different release tool (cargo-release, smart-release). release-plz works fine in registry mode; the upstream bug is in the *non-*registry fallback we've been forced into.
- Promoting any crate past `0.1.0` in this PR. The follow-up `chore: release` PR that release-plz opens after this lands will propose `core 0.1.0 → 0.2.0` (and a facade patch bump via `dependencies_update`) because SMA-318 and SMA-319 are still unreleased.

## 2. Root cause (for future readers)

release-plz determines next versions by comparing the current package state to its last *registry* release. When `publish = false`, no registry comparison happens, and release-plz falls back to walking git history looking for the commit where `cargo package --list` output matched the current state. That walk calls `canonicalize()` on every file in the package list at each historical checkout.

`cargo package --list` always includes `Cargo.lock`. In a Cargo workspace the lockfile lives at the workspace root, not in each crate's directory, so `canonicalize(crate_dir/Cargo.lock)` returns `No such file or directory` at every commit. release-plz can't find a "last release point", silently keeps `next_version = current_version`, and emits `{"prs":[]}`.

The release-plz maintainer confirmed in [issue #2479](https://github.com/release-plz/release-plz/issues/2479) (closed 2025-11-05): *"Release-plz requires publishing to a cargo registry at the moment."* A "git-only" mode is being worked on in upstream PRs #1872 and #2001, neither merged.

The two manual-escape tags (`-core-v0.1.0` at SMA-347, `-sessions-sqlite-v0.1.0` at SMA-382) hide this on the `release` command's side because that command tags whatever's in `Cargo.toml` regardless. The bug surfaces only when a *second* feat lands after the escape — exactly the SMA-318/319 case.

## 3. File layout

```text
release-plz.toml                                    (modified — drop workspace publish=false;
                                                     add 7 release=false [[package]] blocks)
crates/paigasus-helikon-mcp/Cargo.toml              (modified — add publish=false)
crates/paigasus-helikon-tools/Cargo.toml            (modified — add publish=false)
crates/paigasus-helikon-evals/Cargo.toml            (modified — add publish=false)
crates/paigasus-helikon-runtime-tokio/Cargo.toml    (modified — add publish=false)
crates/paigasus-helikon-runtime-axum/Cargo.toml     (modified — add publish=false)
crates/paigasus-helikon-runtime-temporal/Cargo.toml (modified — add publish=false)
crates/paigasus-helikon-runtime-agentcore/Cargo.toml (modified — add publish=false)
CLAUDE.md                                           (modified — Stage 1 done; escape recipe v2)
```

No changes to: any crate's `src/`, the six crates that will publish, the CLI's already-correct `publish = false`, `Cargo.lock` (it'll regenerate naturally), any CI workflow, `deny.toml`, `dependabot.yml`, `.gitattributes`, or scripts.

## 4. Preflight (must happen before merge)

These two checks live outside the diff but block the PR from being merged.

### 4.1 `CARGO_REGISTRY_TOKEN` repo secret

`.github/workflows/release-plz.yml` already wires `CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}` into the `release` step (since SMA-307). The secret has never been set because `publish = false` made it unused. Sven needs to:

1. Create a crates.io API token at <https://crates.io/settings/tokens> with scopes: `publish-new`, `publish-update`. (Least privilege — release-plz never needs to yank or change ownership. If we ever need to yank, do it from a separate short-lived token or the web UI as a deliberate human action.)
2. Add it to the repo as `CARGO_REGISTRY_TOKEN` (Repository secrets, not Environment secrets).

The token is bound to Sven's crates.io account; first publishes will create him as the owner of each crate. He can later `cargo owner --add github:paigasus:publishers` (or whatever org/team structure we end up with) if we want shared ownership.

### 4.2 Name availability

Verify each of the 6 to-be-published names returns 404 from crates.io:

```bash
for name in paigasus-helikon paigasus-helikon-core paigasus-helikon-macros \
            paigasus-helikon-providers-openai paigasus-helikon-providers-anthropic \
            paigasus-helikon-sessions-sqlite; do
  status=$(curl -s -o /dev/null -w '%{http_code}' "https://crates.io/api/v1/crates/$name")
  printf '%s -> %s\n' "$name" "$status"
done
```

Expected output: six `404` lines. If any returns `200`, halt — the name is taken; we'll need to either negotiate ownership or rename. (Given the niche-ness of "paigasus", a conflict is unlikely but not impossible.)

## 5. The diff

### 5.1 `release-plz.toml`

Current:

```toml
[workspace]
dependencies_update = true
publish = false                             # ← REMOVE

[changelog]
sort_commits = "newest"

[[package]]
name = "paigasus-helikon-cli"
publish = false                             # ← keep
```

After:

```toml
[workspace]
dependencies_update = true

[changelog]
sort_commits = "newest"

# Binary-only — never published as a library.
[[package]]
name = "paigasus-helikon-cli"
publish = false

# Stub crates: not on crates.io until they ship real API. The per-package
# publish=false in each Cargo.toml is defense-in-depth (cargo refuses).
# The release=false here makes release-plz skip them entirely — no version
# bump proposals, no CHANGELOG churn, no release PR noise.
[[package]]
name = "paigasus-helikon-mcp"
publish = false
release = false

[[package]]
name = "paigasus-helikon-tools"
publish = false
release = false

[[package]]
name = "paigasus-helikon-evals"
publish = false
release = false

[[package]]
name = "paigasus-helikon-runtime-tokio"
publish = false
release = false

[[package]]
name = "paigasus-helikon-runtime-axum"
publish = false
release = false

[[package]]
name = "paigasus-helikon-runtime-temporal"
publish = false
release = false

[[package]]
name = "paigasus-helikon-runtime-agentcore"
publish = false
release = false
```

The existing inline comment block about the 0.0.0 trap gets rewritten to point at CLAUDE.md's escape recipe (single source of truth), since the trap mechanics now have a second axis (the publish/release toggles).

### 5.2 Stub `Cargo.toml`s

For each of the 7 stub crates, add a single `publish = false` line under `[package]`. Example for `crates/paigasus-helikon-mcp/Cargo.toml`:

```toml
[package]
name        = "paigasus-helikon-mcp"
description = "MCP integration for the Paigasus Helikon AI SDK (stub)."
version                = "0.0.0"
edition.workspace      = true
# ... other workspace.* lines ...
publish                = false                # ← NEW
```

Placement matches the existing per-package metadata block. No other changes to these files.

### 5.3 `CLAUDE.md`

Two edits:

1. **Workspace layout section** — Update "Implementation status" to mention crates.io publishing is live for the 6 real crates; the 7 stubs carry `publish = false` until they ship. Update the description of `release-plz.toml` to reflect the new shape (no workspace-level `publish = false`, seven per-package `release = false` blocks).
2. **0.0.0-trap-escape recipe** (the long paragraph under "Per-crate version is the one exception") — Currently a 3-step recipe (bump Cargo.toml, land as `chore(release)`, let release-plz tag). Replace with a 4-step recipe for stubs that ship real API:
   - Bump `version = "0.0.0"` → `"0.1.0"` in the crate's `Cargo.toml`.
   - Remove `publish = false` from that `Cargo.toml`.
   - Remove the crate's `[[package]] … release = false` block from `release-plz.toml`.
   - Land as one `chore(release): SMA-### lift stage-1 gates for <crate>` commit on the feature branch alongside the implementation. release-plz handles the first publish on CI.

   The legacy recipe (just bump 0.0.0 → 0.1.0, no toggles) no longer applies — all four steps are required.

## 6. First publish flow

Land the PR first. release-plz on the merge-to-main push does the rest, in order:

1. **`release` job.** Sees `Cargo.toml` at `0.1.0` for the 6 publishable crates, sees crates.io has none of them. Publishes all 6 in dependency order (`-core` → `-macros`, `-providers-openai`, `-providers-anthropic`, `-sessions-sqlite` → facade), waiting for the crates.io index to refresh between each. The pre-existing `-v0.1.0` git tags (from the SMA-347/350/372/382 manual escapes plus SMA-317 for anthropic) are reused; release-plz doesn't double-tag.
2. **`release-pr` job.** Sees SMA-318 and SMA-319 as unreleased feat commits for `-core`. Proposes a `chore: release` PR bumping `-core` to `0.2.0` with both entries in the CHANGELOG. The facade gets a patch bump (`0.1.0 → 0.1.1`) via `dependencies_update = true`. The other 4 published crates have no unreleased commits since their 0.1.0 tag, so no bump proposed for them.

**Fallback** if the CI `release` job fails partway (e.g., crates.io rate-limiting on six new publishes in close succession): Sven runs `cargo publish -p <crate>` locally for any crates that didn't make it through, in dependency order, each followed by a ~30s pause for indexing. The PR can be re-pushed (empty commit) to retrigger release-plz; the `release` command is idempotent for already-published versions.

## 7. Acceptance verification

After the PR merges:

1. Six crates exist on crates.io at `0.1.0`. Verified by `cargo search paigasus-helikon` returning all six.
2. A `chore: release` PR opens automatically, proposing `paigasus-helikon-core 0.1.0 → 0.2.0` with the SMA-318 and SMA-319 entries in its CHANGELOG diff. (And likely the facade at `0.1.0 → 0.1.1`.)
3. Merging that follow-up `chore: release` PR causes release-plz to publish `paigasus-helikon-core@0.2.0` to crates.io and create the corresponding git tag and GitHub release.
4. Any future `feat(core):` / `fix(core):` commit triggers a new bump proposal within one workflow run — no manual escape required.

If step 2 doesn't fire within ~5 minutes after merge, check the release-plz workflow logs for `WARN Package` lines or `release_pr_output: {"prs":[]}` — diagnosis path is the same as the local-repro under §2.

## 8. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| One of the 6 names is taken on crates.io | low | §4.2 preflight catches this before merge. Workaround: rename (e.g., `paigasus-helikon-core` → `paigasus-core` and ripple). |
| `CARGO_REGISTRY_TOKEN` scope is wrong (missing `publish-new`) | medium | Token creation explicitly calls out the required scopes. release-plz logs the cargo error verbatim on failure. |
| First publish hits crates.io rate limit | low-medium | release-plz already serializes by dep order; if it still 429s, fall back to local `cargo publish` per §6. |
| Path-only deps block publish | nil | Already handled by SMA-307 — every `[workspace.dependencies]` internal entry sets both `path` and `version`. |
| Stage 1 makes future breaking changes harder | low | We're at 0.x; minor bumps remain breaking until 1.0. crates.io immutability is the right pressure to keep 0.x churn intentional. |
| Releasing 0.1.0 before the API stabilizes signals false maturity | low | This is the standard pre-1.0 contract. Downstream users opt into churn at their own risk; we're not promising stability until we cut 1.0. |
| Stub-toggle drift (publish=false in one file, release=true in the other) | medium over time | §5.3 CLAUDE.md update calls out the coupling. The four-step escape recipe forces both edits together. |

## 9. Out-of-scope follow-ups

- File the upstream release-plz canonicalize bug (own ticket; low priority since we're routing around it).
- Re-evaluate when upstream PR #2001 / #1872 land — we may be able to drop crates.io publishing if "git-only" mode arrives, though there's no reason to once we're already publishing.
- Decide on owner/team structure on crates.io (Sven-only vs `github:paigasus:publishers` team). Not blocking; can be added later via `cargo owner --add`.
- Set up branch protection for `crates.io` releases (e.g., requiring a manual approval before publishing 1.0). Not relevant pre-1.0.
- SBOM workflow's tag-glob trigger (`tags: [v*]` won't fire on release-plz's `<crate>-v*` tags) — documented as a Stage-1 follow-up in the original SMA-307 design but never opened as its own ticket. Worth a separate SMA-* now that Stage 1 is happening.
