# SMA-486 — Design: pin every GitHub Action to a SHA, bump `actions/checkout` to v7.0.1

**Date:** 2026-08-09
**Ticket:** [SMA-486](https://linear.app/smaschek/issue/SMA-486/ci-bump-actionscheckout-to-v7-across-the-remaining-workflows)
**Branch:** `feature/sma-486-ci-bump-actionscheckout-to-v7-across-the-remaining-workflows`
**Base:** `main` @ `abb650b`

## 1. Why this is wider than the ticket title

The ticket was written from PR #181's CodeRabbit comment and describes a single split pin
(`actions/checkout` v6.0.2 vs v7.0.1). Re-reading the tree at implementation time — which the
ticket explicitly asks for ("Re-resolve the latest release at implementation time rather than
trusting the SHA above") — found the scope statement materially stale, and found a second,
worse class of drift the ticket did not know about: **unpinned moving tags**.

### 1.1 Corrected `actions/checkout` inventory

| Workflow | Ticket claims | Actual on `abb650b` | Action |
| --- | --- | --- | --- |
| `ci.yml` | v6.0.2, **6** sites | v6.0.2, **8** sites | bump 8 |
| `docs.yml` | v6.0.2 | v6.0.2 | bump 1 |
| `bench.yml` | v6.0.2 | v6.0.2 | bump 1 |
| `release-plz.yml` | v6.0.2 | v6.0.2 | bump 1 |
| `audit.yml` | v6.0.2 | **already v7.0.1** (2 sites) | none |
| `deny.yml` | v6.0.2 | **already v7.0.1** | none |
| `msrv.yml` | v6.0.2 | **`@v7` — moving tag** | pin |
| `sbom.yml` | v6.0.2 | **`@v7` — moving tag** | pin |
| `integration.yml` | (SMA-457, v7.0.1) | v7.0.1 (2 sites) | none |

`audit.yml` and `deny.yml` were already carried to v7.0.1 by SMA-479. The ticket's "every other
workflow is still on v6.0.2" is therefore wrong in both directions.

The ticket also states `commits`/`msrv` use `fetch-depth: 0`. Only two sites in the repo do:
`ci.yml`'s `commits` job and `release-plz.yml`. `msrv.yml` uses the default depth.

### 1.2 The real finding: moving tags

CLAUDE.md is unambiguous — *"pin to its commit SHA (never a moving `@vN` tag)"*. Nine `uses:`
sites violate this today — four in `msrv.yml`, five in `sbom.yml` — all outside the SMA-457 blast
radius and therefore never reviewed against that rule:

| Site | Ref | Class |
| --- | --- | --- |
| `msrv.yml:19`, `sbom.yml:22` | `actions/checkout@v7` | rolling major |
| `msrv.yml:20`, `sbom.yml:23` | `dtolnay/rust-toolchain@stable` | rolling branch |
| `msrv.yml:26`, `sbom.yml:24` | `Swatinem/rust-cache@v2` | rolling major |
| `msrv.yml:27`, `sbom.yml:25` | `taiki-e/install-action@v2.85.5` | tag, not SHA |
| `sbom.yml:57` | `softprops/action-gh-release@v3` | rolling major |

`msrv.yml` and `sbom.yml` are the two workflows that predate the repo's pinning discipline and
were never swept. `sbom.yml` is the highest-value target of the group: it runs with
`permissions: contents: write` on tag pushes, so a compromised upstream tag there can write
releases.

## 2. Decisions

### D1 — Unify each *touched* action on its latest release SHA

Where a moving tag forces a SHA choice, the sibling SHA-pinned sites of that same action are
brought to the same (latest) version rather than left behind. Pinning `msrv.yml`'s
`rust-cache@v2` to v2.9.2 while nine other sites sit on v2.9.1 would replace one inconsistency
with another. Resolved at implementation time on 2026-08-09:

| Action | Before | After |
| --- | --- | --- |
| `actions/checkout` | v6.0.2 `9c091bb…` / `@v7` | **v7.0.1** `3d3c42e5aac5ba805825da76410c181273ba90b1` |
| `Swatinem/rust-cache` | v2.9.1 `c1937114…` / `@v2` | **v2.9.2** `6323deb102c322ba6fcbdcafc7e3dddab59af2b6` |
| `taiki-e/install-action` | v2.85.7 `67729d5c…`, v2.85.5 `6a1bd70e…`, `@v2.85.5` | **v2.85.11** `7f4eb899022d8fe70b20c4f3de697aa85c309026` |
| `softprops/action-gh-release` | `@v3` | **v3.0.2** `3d0d9888cb7fd7b750713d6e236d1fcb99157228` |

`taiki-e/install-action` was a genuine three-way split (v2.85.5 / v2.85.7 / a bare tag) across
six sites.

### D2 — Do **not** bump `dtolnay/rust-toolchain` to its "latest release"

`gh api repos/dtolnay/rust-toolchain/releases/latest` reports `v1`. Following that blindly would
be a **regression**, and this is the single most important thing recorded in this document:

```
v1        = e97e2d8cc328f1b50210efc529dca0028893a2d9   2025-08-23  "Update actions/checkout@v4 -> v5"
pinned    = 2c7215f132e9ebf062739d9130488b56d53c060c   2026-07-16  "Add 1.97.1 patch release"
master    = 6c977a6ca4077a0ceb28ffbe03f59d46e9ac8772   2026-08-05
```

`compare/v1...pinned` → `ahead_by: 11, behind_by: 0`. The `v1` tag is ~11 months stale; the repo's
existing pin is 11 commits *newer* and sits 2 commits behind `master`. `dtolnay/rust-toolchain`
does not do tagged releases in the usual sense — it publishes rolling **branches** (`stable`,
`nightly`, `master`, `1.0`…`1.14`) and the `v1` tag was cut once and abandoned. The existing
`# dtolnay/rust-toolchain master (no tagged releases)` comment is a correct, deliberate strategy.

**Consequence:** the 12 existing `2c7215f…` sites are left untouched, and the two `@stable` sites
are pinned *to that same SHA* rather than to `v1`. The comments are reworded to say why, so the
next contributor running the CLAUDE.md recipe does not "fix" this into a downgrade.

### D3 — `@stable` → SHA requires an explicit `toolchain:` input

`dtolnay/rust-toolchain` infers the toolchain from the ref it was invoked as
(`GITHUB_ACTION_REF`). `uses: dtolnay/rust-toolchain@stable` means *install stable*; a SHA ref
carries no such meaning. Every SHA-pinned site in the repo already pairs the pin with
`with: toolchain: …`. So pinning `msrv.yml:20` / `sbom.yml:23` must add `with: toolchain: stable`
or the step breaks. This is the one edit in this PR that is not purely textual.

### D4 — Add `persist-credentials: false` to the two swept checkouts

The repo has 18 checkout sites; the other 16 all set it, and `msrv.yml`/`sbom.yml` are the only
omissions. Neither
job pushes or otherwise uses the git credential afterwards (`cargo msrv verify`;
`cargo cyclonedx` + an API-based release upload), so persisting the token is exposure with no
benefit — and `sbom.yml` holds `contents: write`. Confirmed in scope with the ticket owner.

### D5 — Leave `dorny/paths-filter` alone; fix only its comment

Its three sites all agree on `7b450ff…` and none is a moving tag, so nothing forces a choice
(cf. D1). Patch/minor drift there is exactly what Dependabot's `github-actions` group is for —
that group is *only* blind to majors, which is why the checkout v6→v7 gap survived. The
above-the-fold comment is wrong, though: it reads `v4.0.1` while the SHA is `v4.0.2`. Correcting
a factually false audit comment is a strict improvement; bumping to v4.0.3 is not this PR's job.

### D6 — `pr-title.yml` still gets no checkout

Unchanged, per the ticket. It runs on `pull_request_target`; keeping PR-controlled code off that
runner is the point.

## 3. Risk analysis

**`actions/checkout` v7.0.0 has exactly one breaking change:** *"block checking out fork PR for
`pull_request_target` and `workflow_run`"*. Audited every trigger in the repo — `pr-title.yml` is
the only `pull_request_target` workflow and it has no checkout step; **no** workflow uses
`workflow_run`. Not affected. v7.0.1 is bugfix-only (unsafe-PR-check skip, branch-name trimming,
`--unset` escaping).

`fetch-depth: 0` and `persist-credentials` semantics are untouched between v6 and v7, so
`release-plz.yml` (needs full history for its changelog walk) and `ci.yml`'s `commits` job
(`convco check` over the PR range) keep working. v7.0.1 is already proven green in this repo on
`integration.yml`, `audit.yml`, and `deny.yml`.

Residual risk is low and concentrated in `sbom.yml`, which is the only changed workflow **not**
exercised by this PR's own CI — it triggers on `paigasus-helikon-v*` tag pushes only. Its edits
are the largest of the sweep (five refs plus a new input). See the plan's verification step for
how that is mitigated without cutting a release.

## 4. Out of scope

- Bumping `dorny/paths-filter` v4.0.2 → v4.0.3 (D5).
- Any change to `Cargo.toml`/`crates/**`. A `ci`-typed PR touching only `.github/**` attributes
  nothing to any crate, so release-plz will not open a release PR — unlike SMA-457.
- No mdBook or crate-README edit: pure CI plumbing, no user-facing surface. Conscious call per
  CLAUDE.md, not a silent skip.
