# SMA-486 — Plan: pin every GitHub Action to a SHA, bump `actions/checkout` to v7.0.1

**Design:** [`docs/superpowers/specs/2026-08-09-sma-486-action-pin-sweep-design.md`](../specs/2026-08-09-sma-486-action-pin-sweep-design.md)
**Date:** 2026-08-09

Resolved SHAs (all re-resolved 2026-08-09; see design §2 D1):

| Action | Version | SHA |
| --- | --- | --- |
| `actions/checkout` | v7.0.1 | `3d3c42e5aac5ba805825da76410c181273ba90b1` |
| `Swatinem/rust-cache` | v2.9.2 | `6323deb102c322ba6fcbdcafc7e3dddab59af2b6` |
| `taiki-e/install-action` | v2.85.11 | `7f4eb899022d8fe70b20c4f3de697aa85c309026` |
| `softprops/action-gh-release` | v3.0.2 | `3d0d9888cb7fd7b750713d6e236d1fcb99157228` |
| `dtolnay/rust-toolchain` | master @ 2026-07-16 | `2c7215f132e9ebf062739d9130488b56d53c060c` (unchanged — D2) |

---

## Task 1 — `actions/checkout` v6.0.2 → v7.0.1 (11 sites)

Purely mechanical: replace `9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0` with the v7.0.1 SHA and
retitle each above-the-fold `# actions/checkout v6.0.2` comment to `v7.0.1`.

- `ci.yml` — 8 sites (L32, L46, L80, L101, L130, L154, L177, L191)
- `docs.yml` — L24
- `bench.yml` — L14
- `release-plz.yml` — L24

Do **not** touch the `with:` blocks. `fetch-depth: 0` at `ci.yml:179` (`commits`) and
`release-plz.yml:26` stays; `persist-credentials: false` stays everywhere.

No edit to `audit.yml`, `deny.yml`, `integration.yml` — already v7.0.1.

**Verify:** `grep -c 9c091bb21b7c1c1d1991bb908d89e4e9dddfe3e0 .github/workflows/*.yml` → 0 matches.

## Task 2 — Pin `msrv.yml`'s five refs

Rewrite the step list to match the pinning conventions used by `ci.yml`:

- `actions/checkout@v7` → v7.0.1 SHA + version comment + `with: persist-credentials: false` (D4)
- `dtolnay/rust-toolchain@stable` → `2c7215f…` + **`with: toolchain: stable`** (D3 — load-bearing)
- `Swatinem/rust-cache@v2` → v2.9.2 SHA + comment
- `taiki-e/install-action@v2.85.5` → v2.85.11 SHA + comment (keep `with: tool: cargo-msrv`)
- `arduino/setup-protoc` — already pinned, leave it and its SMA-332 comment alone

Leave the `cargo msrv` step and its "no `--workspace` flag" comment untouched.

**Verify:** `with: toolchain: stable` present; step count unchanged at 5.

## Task 3 — Pin `sbom.yml`'s five refs

Same treatment, plus the release upload:

- `actions/checkout@v7` → v7.0.1 SHA + `persist-credentials: false`
- `dtolnay/rust-toolchain@stable` → `2c7215f…` + `with: toolchain: stable`
- `Swatinem/rust-cache@v2` → v2.9.2 SHA
- `taiki-e/install-action@v2.85.5` → v2.85.11 SHA (keep `with: tool: cargo-cyclonedx`)
- `softprops/action-gh-release@v3` → v3.0.2 SHA (keep `files:` / `fail_on_unmatched_files:`)

`permissions: contents: write` and the `--spec-version 1.5` / `find` logic are untouched — the
`cargo-cyclonedx` reasoning comments stay exactly as they are.

**Verify:** the three run-step bodies are byte-identical to `main`; only `uses:`/`with:` changed.

## Task 4 — Unify `rust-cache` + `install-action` on the sibling sites

Bring the already-pinned sites to the same versions chosen in Tasks 2–3, so the repo carries one
version per action (D1):

- `Swatinem/rust-cache` `c19371144df3bb44fab255c43d04cbc2ab54d1c4` (v2.9.1) → v2.9.2 SHA.
  Sites: `ci.yml` ×6, `audit.yml`, `deny.yml`, `bench.yml`, `integration.yml`.
- `taiki-e/install-action` → v2.85.11 SHA. Sites: `audit.yml` + `deny.yml` (were v2.85.7),
  `ci.yml:182` + `docs.yml:28` (were v2.85.5).

Retitle every corresponding `# <action> vX.Y.Z` comment. Several of these comments are already
stale relative to their own SHA (e.g. `docs.yml` says v2.79.3, `ci.yml` says v2.79.0, both pinning
v2.85.5) — fix them to the truth.

Also fix the `dorny/paths-filter` comment: reads `v4.0.1`, SHA is `v4.0.2` (D5). SHA unchanged.

**Verify:** `grep -rc c19371144df3\|67729d5c413d\|6a1bd70eaac3 .github/workflows/` → 0.

## Task 5 — Verification gate

Run all of these before committing; every one must pass.

1. **No moving refs anywhere.** Every `uses:` value must be `owner/repo@<40-hex>`:
   ```bash
   grep -rhoE 'uses: [^ ]+' .github/workflows/ | grep -v 'uses: \./' \
     | grep -vE '@[0-9a-f]{40}$' || echo "OK: all pinned"
   ```
2. **Every SHA matches its comment.** Walk each `# <owner/repo> vX.Y.Z` / `uses:` pair and confirm
   via `gh api repos/<owner>/<repo>/git/ref/tags/<tag>` that the tag resolves to the pinned SHA
   (dereferencing annotated tags). `dtolnay/rust-toolchain` is exempt — its comment names a branch
   and a date, not a tag (D2).
3. **YAML parses** for all nine workflows.
4. **`actionlint`** clean if available (`brew install actionlint`); it independently flags unpinned
   actions and bad `with:` keys — notably it would catch a missing `toolchain:` input.
5. **`sbom.yml` cannot be exercised by CI** (tag-triggered). Compensate by diffing it against
   `main` to prove only `uses:`/`with:` lines moved, and by confirming the v3.0.2 SHA is reachable
   on the release tag. Do not cut a throwaway tag.

## Task 6 — Commit, PR, ticket hygiene

Commits (`ci` type, `workflows` scope — both allowlisted in `.versionrc` and `pr-title.yml` on
`main`, so the PR title is safe; cf. the SMA-343 scope trap):

1. `ci(workflows): SMA-486 bump actions/checkout to v7.0.1 across remaining workflows`
2. `ci(workflows): SMA-486 pin msrv and sbom moving tags to commit SHAs`
3. `ci(workflows): SMA-486 unify rust-cache and install-action pins`
4. `docs(specs): SMA-486 add action pin sweep design and plan`

PR title: `ci(workflows): SMA-486 pin every action to a SHA and bump checkout to v7.0.1`
— full Conventional Commits prefix, lowercase subject after `SMA-486 `.

Then: update the SMA-486 description in Linear so it reflects the corrected inventory and the
widened scope (the stale "every other workflow is on v6.0.2" claim actively misleads a reviewer).

Merge is a **user** action — agent-authored PRs cannot self-approve past `main-protection-reviews`.
Hand off `gh pr merge <n> --squash --admin --delete-branch`.

Expect **no** release-plz PR: `ci`-typed, `.github/**`-only, attributes to no crate.
