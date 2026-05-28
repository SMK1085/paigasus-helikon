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
2. The seven docstring-only stubs (`-mcp`, `-tools`, `-evals`, `-runtime-{tokio,axum,temporal,agentcore}`) are pre-published at `0.0.0` as name-claim placeholders, then pinned with per-package `publish = false` *and* `release = false` so cargo refuses further publishes and release-plz ignores them entirely. Pre-publishing is necessary because the facade declares them as optional deps with `version = "0.0.0"` — cargo's publish resolver refuses the facade until these references resolve on crates.io.
3. `paigasus-helikon-cli` gets `publish = false` added to its `Cargo.toml` for parity with the stubs (it already had this in `release-plz.toml`; the per-`Cargo.toml` flag is defense-in-depth against accidental ad-hoc publishes).
4. The macros↔facade dev-dep cycle is broken by moving one trybuild test (`facade_only_consumer.rs`) from `-macros` to the facade. Necessary because cargo's publish resolver also checks dev-deps; without the move, `cargo publish -p paigasus-helikon-macros` fails on `no matching package named 'paigasus-helikon'`.
5. The SBOM workflow's tag-glob is corrected from `v*` to `paigasus-helikon-v*` so it fires on the facade's release-plz tags. The current glob has been dead since SMA-307 landed because release-plz emits crate-prefixed tags.
6. CLAUDE.md is updated to reflect that Stage 1 has happened, and the 0.0.0-trap-escape recipe gains a fourth step covering the new per-package toggles.

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
release-plz.toml                                                          (modified — drop workspace publish=false; add 7 release=false [[package]] blocks)
crates/paigasus-helikon-mcp/Cargo.toml                                    (modified — add publish=false)
crates/paigasus-helikon-tools/Cargo.toml                                  (modified — add publish=false)
crates/paigasus-helikon-evals/Cargo.toml                                  (modified — add publish=false)
crates/paigasus-helikon-runtime-tokio/Cargo.toml                          (modified — add publish=false)
crates/paigasus-helikon-runtime-axum/Cargo.toml                           (modified — add publish=false)
crates/paigasus-helikon-runtime-temporal/Cargo.toml                       (modified — add publish=false)
crates/paigasus-helikon-runtime-agentcore/Cargo.toml                      (modified — add publish=false)
crates/paigasus-helikon-cli/Cargo.toml                                    (modified — add publish=false for parity)
crates/paigasus-helikon-macros/Cargo.toml                                 (modified — drop dev-dep on paigasus-helikon)
crates/paigasus-helikon-macros/tests/trybuild.rs                          (modified — remove the facade_only_consumer line)
crates/paigasus-helikon-macros/tests/ui/facade_only_consumer.rs           (deleted — moved to facade)
crates/paigasus-helikon/Cargo.toml                                        (modified — add trybuild + schemars + serde dev-deps)
crates/paigasus-helikon/tests/trybuild.rs                                 (new — runner)
crates/paigasus-helikon/tests/ui/facade_only_consumer.rs                  (new — moved from macros)
.github/workflows/sbom.yml                                                (modified — tags glob: ["v*"] → ["paigasus-helikon-v*"])
CLAUDE.md                                                                 (modified — Stage 1 done; escape recipe v2)
```

No changes to: any non-test `src/`, the six crates' versions, `Cargo.lock` (it'll regenerate naturally), `ci.yml`, `msrv.yml`, `audit.yml`, `deny.yml`, `release-plz.yml`, `deny.toml`, `dependabot.yml`, `.gitattributes`, or scripts.

## 4. Preflight (must happen before merge)

Four steps run outside the diff before the PR is mergeable. Two are passive checks (§4.1 token, §4.2 names); two are diagnostic/mutating (§4.3 baseline dry-run, §4.4 stub pre-publish). Linear order matters — see the rollout sequence in §4.4.

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

Repeat the check for each of the 7 stubs (`-mcp`, `-tools`, `-evals`, `-runtime-tokio`, `-runtime-axum`, `-runtime-temporal`, `-runtime-agentcore`). Same expected output. The stub names need claiming too because of §4.4 below.

### 4.3 `cargo publish --dry-run` diagnostic baseline

Run dry-runs on all 13 crates against current `main` (no §5 changes, no §4.4 publishes yet):

```bash
for crate in paigasus-helikon-core paigasus-helikon-macros \
             paigasus-helikon-providers-openai paigasus-helikon-providers-anthropic \
             paigasus-helikon-sessions-sqlite paigasus-helikon \
             paigasus-helikon-mcp paigasus-helikon-tools paigasus-helikon-evals \
             paigasus-helikon-runtime-tokio paigasus-helikon-runtime-axum \
             paigasus-helikon-runtime-temporal paigasus-helikon-runtime-agentcore; do
  echo "=== $crate ==="
  cargo publish --dry-run -p "$crate" --allow-dirty 2>&1 | tail -6
done
```

**Expected** baseline (verified locally 2026-05-28 during spec writing):

| Crate | Result | Why |
|---|---|---|
| `-core` | ✅ pass | No internal deps |
| `-macros` | ❌ fails on `paigasus-helikon` | dev-dep cycle — §5.5 fixes |
| `-providers-openai`, `-providers-anthropic`, `-sessions-sqlite` | ❌ fail on `paigasus-helikon-core` | Resolves after release-plz publishes core post-merge |
| facade | ❌ fails on `paigasus-helikon-core` (and would then fail on stubs) | Resolves after release-plz publishes core, deps already-published stubs from §4.4 |
| 7 stubs | ✅ pass | No internal deps; this is what makes §4.4 possible |

If anything else fails (e.g., a stub fails) or anything is missing from the expected-failure column, halt and investigate.

After applying §5.5 (remove the macros dev-dep on facade), re-run the macros dry-run alone — it should now fail only on `paigasus-helikon-core` (the ordering issue, not the cycle). That confirms §5.5 is sufficient.

### 4.4 Pre-publish each stub at `0.0.0`

The seven stubs need to exist on crates.io as `0.0.0` placeholders before the facade can publish. Each is docstring-only (no internal deps; tiny tarball):

```bash
for stub in paigasus-helikon-mcp paigasus-helikon-tools paigasus-helikon-evals \
            paigasus-helikon-runtime-tokio paigasus-helikon-runtime-axum \
            paigasus-helikon-runtime-temporal paigasus-helikon-runtime-agentcore; do
  cargo publish -p "$stub"     # NO --dry-run; this is the real publish
  sleep 30                     # crates.io index refresh latency
done
```

This step runs **before** the §5 changes are applied to the stubs' `Cargo.toml`s — because §5 adds `publish = false` to each stub, which would make cargo refuse the publish. Linear order across the whole rollout:

```
§4.1 + §4.2 (token, names available)
  → §4.3 (baseline dry-run, expected-failure pattern matches)
  → §4.4 (publish 7 stubs at 0.0.0)
  → §5 (apply all file changes on the branch)
  → push, PR, merge
  → release-plz publishes the 6 real crates on the main push
```

The `0.0.0` versions become permanent name-claim entries on crates.io. Each is ~1 KB of compiled docstring; the namespace cost is trivial. When a stub later ascends to real API, the 4-step recipe (§5.3) publishes a real `0.1.0` over the `0.0.0` placeholder.

This step also requires `cargo login` (or `CARGO_REGISTRY_TOKEN` env var) on the local machine. Sven uses his crates.io account; the token can be the same one as §4.1 or a separate short-lived one.

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

1. **Workspace layout section** — Update "Implementation status" to mention crates.io publishing is live for the 6 real crates; the 7 stubs are pre-published at `0.0.0` as name-claim placeholders and carry `publish = false` until they ship real API. Update the description of `release-plz.toml` to reflect the new shape (no workspace-level `publish = false`, seven per-package `release = false` blocks).
2. **0.0.0-trap-escape recipe** (the long paragraph under "Per-crate version is the one exception") — Currently a 3-step recipe (bump Cargo.toml, land as `chore(release)`, let release-plz tag). Replace with a 4-step recipe for stubs that ship real API:
   - Bump `version = "0.0.0"` → `"0.1.0"` in the crate's `Cargo.toml`.
   - Remove `publish = false` from that `Cargo.toml`.
   - Remove the crate's `[[package]] … release = false` block from `release-plz.toml`.
   - Land as one `chore(release): SMA-### lift stage-1 gates for <crate>` commit on the feature branch alongside the implementation. release-plz handles the first publish on CI.

   The 4-step recipe applies to **stubs ascending from `0.0.0`**. The six already-released crates (`-core`, facade, `-macros`, `-providers-openai`, `-providers-anthropic`, `-sessions-sqlite`) ship through release-plz's normal flow after SMA-385 lands — no manual ritual needed for their future bumps. The historical chain of `chore(release): … escape release-plz 0.0.0 trap …` commits in the git log (SMA-317/347/350/372/382) is pre-Stage-1 archaeology and won't recur for those crates.

### 5.4 `paigasus-helikon-cli/Cargo.toml`

Add a single `publish = false` line under `[package]`, matching the pattern from §5.2:

```toml
[package]
name     = "paigasus-helikon-cli"
description = "CLI binaries (helikon, paigasus-helikon) for the Paigasus Helikon AI SDK."
autobins = false
version                = "0.0.0"
edition.workspace      = true
# ... other workspace.* lines ...
publish                = false                # ← NEW
```

Defense-in-depth — the CLI's `[[package]] publish = false` in `release-plz.toml` already keeps release-plz from publishing it, but the per-`Cargo.toml` flag makes cargo refuse an accidental ad-hoc `cargo publish -p paigasus-helikon-cli`. Parity with the stub treatment.

### 5.5 Break the macros↔facade dev-dep cycle

`crates/paigasus-helikon-macros/Cargo.toml` declares `paigasus-helikon = { workspace = true, features = ["macros"] }` as a `[dev-dependencies]`. This dev-dep exists to support a single trybuild test (`tests/ui/facade_only_consumer.rs`) that pins the macro's facade-path resolution (when the caller depends only on `paigasus-helikon`, the macro must emit `::paigasus_helikon::core::…`). cargo's resolver runs at publish time and treats unresolved dev-deps as a fatal error, so this dev-dep makes macros unpublishable as long as the facade isn't on the registry — and the facade depends on macros (optional regular), creating a publish-time cycle.

Resolution: move the test to the facade crate.

**`crates/paigasus-helikon-macros/Cargo.toml`** — remove only the facade dev-dep line from `[dev-dependencies]`:

```toml
paigasus-helikon = { workspace = true, features = ["macros"] }   # ← DELETE
```

Keep `paigasus-helikon-core = { workspace = true }` — the other macros tests (`schema_golden.rs`, `end_to_end.rs`, the trybuild `ui/bad_*.rs` set) use `paigasus_helikon_core::…` directly. That dev-dep is publish-safe because core has no internal deps and release-plz publishes it first; by the time macros publishes, core is on crates.io.

**`crates/paigasus-helikon-macros/tests/trybuild.rs`** — remove the one line:

```rust
t.pass("tests/ui/facade_only_consumer.rs");
```

**`crates/paigasus-helikon-macros/tests/ui/facade_only_consumer.rs`** — delete.

**`crates/paigasus-helikon/tests/ui/facade_only_consumer.rs`** — create with the moved content. Same source as the deleted file; `use paigasus_helikon::core::{Tool, ToolContext, ToolError};` already works because the facade re-exports core unconditionally and macros via the `macros` feature.

**`crates/paigasus-helikon/tests/trybuild.rs`** — create:

```rust
//! UI tests for facade-only macro consumption. See SMA-385 spec §5.5 for
//! why this lives in the facade rather than -macros.
#[test]
fn trybuild_ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/facade_only_consumer.rs");
}
```

**`crates/paigasus-helikon/Cargo.toml`** — add a `[dev-dependencies]` block (the existing one has the cargo-husky hook installer; extend it):

```toml
[dev-dependencies]
cargo-husky = { version = "1", default-features = false, features = ["user-hooks"] }
trybuild   = { workspace = true }
schemars   = { workspace = true }
serde      = { workspace = true }
serde_json = { workspace = true }
```

The `trybuild` test must run under `--features macros` (because the test file uses `paigasus_helikon::{tool, tools}`). The `.github/workflows/ci.yml` test matrix runs `cargo test --workspace --all-features`, so coverage is preserved. No CI changes needed.

### 5.6 `.github/workflows/sbom.yml` — fix the tag glob

Line 6 currently reads `tags: ["v*"]`. release-plz emits per-crate tags like `paigasus-helikon-core-v0.2.0`, none of which start with `v`. The workflow has been dead since SMA-307 landed.

Replace with:

```yaml
on:
  push:
    tags:
      - "paigasus-helikon-v*"
```

This matches **only** the facade's tags (`paigasus-helikon-v0.1.0`, not `paigasus-helikon-core-v0.1.0` — globs are anchored to literal `v` after the dash). The facade gets a patch bump on every internal-dep version change via `dependencies_update = true`, so every workspace state change produces exactly one SBOM. Cleaner than firing on every per-crate tag.

## 6. First publish flow

Sequence:

1. **Preflight §4.4 runs first**, manually from Sven's machine. Stubs land on crates.io at `0.0.0`.
2. **§5 diffs land** on the feature branch (this includes adding `publish = false` to the stubs' `Cargo.toml`s, which would now fail if §4.4 hadn't already published them).
3. **PR merges to main.**
4. **release-plz `release` job** sees Cargo.toml at `0.1.0` for the 6 publishable crates, sees crates.io has none of them. Publishes all 6 in dependency order (`-core` → `-macros` + `-providers-openai` + `-providers-anthropic` + `-sessions-sqlite` (parallel-safe) → facade), waiting for the crates.io index to refresh between each. The pre-existing `-v0.1.0` git tags (from the SMA-347/350/372/382 manual escapes plus SMA-317 for anthropic) are reused; release-plz doesn't double-tag.
5. **release-plz `release-pr` job** sees SMA-318 and SMA-319 as unreleased feat commits for `-core`. Proposes a `chore: release` PR bumping `-core` to `0.2.0` with both entries in the CHANGELOG. The facade gets a patch bump (`0.1.0 → 0.1.1`) via `dependencies_update = true`. The other 4 published crates have no unreleased commits since their 0.1.0 tag, so no bump proposed for them.

**Fallback** if the CI `release` job fails partway (e.g., crates.io rate-limiting on six new publishes in close succession): Sven runs `cargo publish -p <crate>` locally for any crates that didn't make it through, in dependency order, each followed by a ~30s pause for indexing. Requires `cargo login` or `CARGO_REGISTRY_TOKEN` in his local env. The PR can be re-pushed (empty commit) to retrigger release-plz; the `release` command is idempotent for already-published versions.

## 7. Acceptance verification

Checkpoint A — after §4.4 runs (pre-merge):

- Seven stubs exist on crates.io at `0.0.0`. Verified by `cargo search paigasus-helikon-mcp` (and the other six) showing each entry.

Checkpoint B — after SMA-385 PR merges:

- Six real crates exist on crates.io at `0.1.0`. Verified by `cargo search paigasus-helikon` returning all six new entries.
- A `chore: release` PR opens automatically, proposing `paigasus-helikon-core 0.1.0 → 0.2.0` with the SMA-318 and SMA-319 entries in its CHANGELOG diff, and the facade at `0.1.0 → 0.1.1` via `dependencies_update`.

Checkpoint C — after the follow-up `chore: release` PR merges:

- release-plz publishes `paigasus-helikon-core@0.2.0` and `paigasus-helikon@0.1.1` to crates.io, creates the matching git tags (`paigasus-helikon-core-v0.2.0`, `paigasus-helikon-v0.1.1`), and creates the corresponding GitHub releases.
- The SBOM workflow fires on the `paigasus-helikon-v0.1.1` tag push and attaches a CycloneDX SBOM to the GitHub release. This confirms the §5.6 glob fix (the pre-existing `paigasus-helikon-v0.1.0` tag was pushed before this PR, so it doesn't trigger).

Checkpoint D — ongoing:

- Any future `feat(core):` / `fix(core):` commit on `main` triggers a new bump proposal within one workflow run — no manual escape required.

If the Checkpoint B `chore: release` PR doesn't fire within ~5 minutes after merge, check the release-plz workflow logs for `WARN Package` lines or `release_pr_output: {"prs":[]}` — diagnosis path is the same as the local-repro under §2.

## 8. Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| One of the 13 names is taken on crates.io | low | §4.2 preflight catches this before merge. Workaround: rename (e.g., `paigasus-helikon-core` → `paigasus-core` and ripple). |
| `CARGO_REGISTRY_TOKEN` scope is wrong (missing `publish-new`) | medium | Token creation explicitly calls out the required scopes. release-plz logs the cargo error verbatim on failure. |
| First publish hits crates.io rate limit | low-medium | release-plz already serializes by dep order; if it still 429s, fall back to local `cargo publish` per §6. §4.4 pre-publishes are deliberately spaced 30s apart for the same reason. |
| Path-only deps block publish | nil | Already handled by SMA-307 — every `[workspace.dependencies]` internal entry sets both `path` and `version`. |
| `cargo publish --dry-run` doesn't catch all publish-time failures | low | We rely on the dry-run to be representative. The known asymmetry: dry-run does NOT actually push to crates.io, so failures only surface as registry-side validation (e.g., name-claim race against another user publishing the same name in the gap between preflight and §4.4). Mitigation: §4.2 name-availability is checked at preflight time *and* the §4.4 publishes happen within minutes of the check. |
| Stub-toggle drift (publish=false in one file, release=true in the other) | medium over time | §5.3 CLAUDE.md update calls out the coupling. The 4-step escape recipe forces both edits together. |
| The 7 stub `0.0.0` versions clutter crates.io forever | nil | Each is ~1 KB; namespace cost is trivial. Yanking later (post-stub-ascend) is optional but discouraged; cleanliness isn't worth the risk of breaking downstream pin behavior. |
| Moving the trybuild test from macros to facade reduces coverage signal | low | Coverage is preserved — the test still runs as part of `cargo test --workspace --all-features` in CI. The path it exercises (facade-only resolution of `proc-macro-crate`) is unchanged. |
| Releasing 0.1.0 before the API stabilizes signals false maturity | low | Standard pre-1.0 contract. Downstream users opt into churn at their own risk; we're not promising stability until we cut 1.0. |

## 9. Out-of-scope follow-ups

- File the upstream release-plz canonicalize bug (own ticket; low priority since we're routing around it).
- Re-evaluate when upstream PR #2001 / #1872 land — we may be able to drop crates.io publishing if "git-only" mode arrives, though there's no reason to once we're already publishing.
- Decide on owner/team structure on crates.io (Sven-only vs `github:paigasus:publishers` team). Not blocking; can be added later via `cargo owner --add`.
- Set up branch protection for `crates.io` releases (e.g., requiring a manual approval before publishing 1.0). Not relevant pre-1.0.
