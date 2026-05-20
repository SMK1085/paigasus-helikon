# SMA-310 — Repo hygiene: README, LICENSE, CONTRIBUTING, templates — design

- **Linear:** [SMA-310](https://linear.app/smaschek/issue/SMA-310/repo-hygiene-readme-license-contributing-templates)
- **Branch:** `feature/sma-310-repo-hygiene-readme-license-contributing-templates`
- **Status:** design (awaiting implementation plan)
- **Author:** Sven Maschek
- **Date:** 2026-05-20

## 1. Goal

Land the baseline open-source hygiene files so external contributors can submit PRs against `paigasus-helikon` without guessing at conventions, licenses, security disclosure, or report formats. The end state matches GitHub's "Community Standards" checklist (README, license, code of conduct, contributing guide, security policy, issue templates, PR template) at 100%.

A secondary deliverable is reversing the 2026-05-16 "MIT-only" licensing decision in favor of the Rust ecosystem-standard `Apache-2.0 OR MIT` dual license. The reversal lands in this same PR because every file added by this ticket references licensing, and a split would leave the README and `CONTRIBUTING.md` referencing the wrong terms for one PR's worth of history.

## 2. Decisions and rationale

Six decisions were made during brainstorming. They shape the rest of the spec.

| Decision | Choice | Rationale |
|---|---|---|
| Licensing | **Dual-license `Apache-2.0 OR MIT`** (reverses the 2026-05-16 MIT-only call) | Rust ecosystem convention. Matches every major workspace dependency, simplifies relicensing-pressure conversations downstream, and is what the Linear ticket originally asked for. |
| Sequencing | **One PR for all hygiene changes** | Matches the ticket's "baseline so contributors can land" framing. Splitting the licensing change into a separate PR was considered (option C in brainstorming) but rejected: the README + `CONTRIBUTING.md` text both reference the license, so a split would land inconsistent docs for at least one PR's worth of history. |
| README scope | **Ticket-as-written, full polish** (~70–80 lines) | Codename story, pitch, install snippet, Notion + Linear links, status note, badges. No crates.io / docs.rs badges yet (nothing published); `<!-- TODO -->` comments stake the slots. No FAQ / provider matrix until crates have real APIs. |
| Security reporting channel | **GitHub Private Security Advisories only** | Single audited channel, no inbox to expose to spam, plays nicely with the existing `cargo audit` + RustSec workflows already in place. |
| CoC enforcement contact | **`dev@paigasus.com`** | Project-scoped alias rather than the maintainer's personal inbox. Matches the kind of separation Contributor Covenant 2.1 expects. |
| Issue templates | **Three templates as ticket-specified** (bug_report, feature_request, config.yml) | No `docs.yml` or `security.yml` redirect — a label on `feature_request.yml` covers docs, and `config.yml`'s `contact_links` covers the security-redirect case without adding a separate template. |
| PR template | **Lean (~25 lines)** | Summary + linked SMA + type-of-change pointer to Conventional Commits + checklist. Verbose templates (reviewer focus areas, release-notes drafts) add friction without payoff at this stage; Conventional Commits + CI gates cover most enforcement. |

A natural consequence of the licensing decision is that **`CLAUDE.md` must be edited in the same PR**: it currently encodes the MIT-only rule as a hard non-obvious convention, and leaving it stale would actively mislead the next contributor (human or agent). The rule is replaced, not removed.

## 3. Files added / modified

### Added

| Path | Purpose |
|---|---|
| `LICENSE-APACHE` | Apache License 2.0 standard text (https://www.apache.org/licenses/LICENSE-2.0.txt) with `Copyright 2026 Sven Maschek` in the boilerplate. |
| `CODE_OF_CONDUCT.md` | Contributor Covenant 2.1 verbatim with `dev@paigasus.com` as the enforcement contact. CC BY 4.0 attribution footer kept (required by the Covenant's own license). |
| `SECURITY.md` | Supported-versions table, PSA-only reporting flow, timing commitments, scope notes. |
| `.github/ISSUE_TEMPLATE/bug_report.yml` | GitHub form template; title prefix `bug: `; labels `type:bug`, `status:triage`. |
| `.github/ISSUE_TEMPLATE/feature_request.yml` | GitHub form template; title prefix `feat: `; labels `type:enhancement`, `status:triage`. |
| `.github/ISSUE_TEMPLATE/config.yml` | `blank_issues_enabled: false` plus `contact_links` for Linear (tracked work) and GitHub PSA (security). |
| `.github/PULL_REQUEST_TEMPLATE.md` | Lean ~25-line template. See §7. |

### Renamed

| From | To | Notes |
|---|---|---|
| `LICENSE` | `LICENSE-MIT` | `git mv` to preserve history. Content unchanged (same MIT text, same copyright line). |

### Modified

| Path | Change |
|---|---|
| `Cargo.toml` (root) | `[workspace.package] license = "MIT"` → `license = "Apache-2.0 OR MIT"`. No per-crate `Cargo.toml` edits; all crates inherit via `license.workspace = true`. |
| `CLAUDE.md` | Replace the "License is MIT only (decided 2026-05-16)" bullet with the dual-license rule + reversal note (verbatim wording in §4). |
| `README.md` | Rewrite from current 18-line stub to ~70–80-line landing page. Outline in §5. |
| `CONTRIBUTING.md` | Three surgical additions (Code of Conduct pointer, Security pointer, dual-license phrasing in any sentence currently saying "MIT-licensed"). No restructuring. |

## 4. Licensing change

### 4.1 Workspace metadata diff

Root `Cargo.toml`, `[workspace.package]`:

```diff
- license       = "MIT"
+ license       = "Apache-2.0 OR MIT"
```

The SPDX expression `Apache-2.0 OR MIT` is the de facto standard for Rust libraries and is recognized by `cargo-deny`, `cargo-license`, crates.io, and downstream license scanners. No per-crate `Cargo.toml` change is required because every member crate already declares `license.workspace = true` (workspace inheritance is mandatory per CLAUDE.md).

### 4.2 Commit type

The Cargo.toml edit lands as `chore(license): SMA-310 ...`, not `feat`/`fix`. This is the SMA-307 rule: workspace-metadata changes that touch the root `Cargo.toml` propagate to every crate via inheritance, and release-plz would otherwise attribute a version bump to every member crate. `chore` is parsed by Conventional Commits as a non-version-bumping change, which is correct — a license-string update is not a release-worthy change.

### 4.3 CLAUDE.md replacement text

Replace this bullet:

> *License is MIT only (decided 2026-05-16). Don't add `LICENSE-APACHE` or set `license = "Apache-2.0 OR MIT"` even though the Cargo ecosystem convention is dual-licensing.*

with:

> *License is dual `Apache-2.0 OR MIT` (decided 2026-05-20, reversing the 2026-05-16 MIT-only call). Both `LICENSE-APACHE` and `LICENSE-MIT` live at the repo root; the workspace metadata is `license = "Apache-2.0 OR MIT"`. Per Rust ecosystem convention — no Apache-only or MIT-only crates in the workspace. Contributions are accepted under the same dual license by default (standard Apache-2.0 §5 inbound-equals-outbound clause restated in the README).*

### 4.4 `cargo-deny` interaction

`deny.toml`'s license allowlist already permits `Apache-2.0` and `MIT` for transitive dependencies, so the relicense does not require a `deny.toml` change. The `deny` CI job will re-run as a smoketest on the PR; expected to remain green.

## 5. README outline

Target: ~70–80 lines, Markdown only (no HTML beyond inline badge SVGs from shields.io). Sections in order:

1. **Title** — `# paigasus-helikon`.
2. **Tagline** — one line: "Paigasus AI SDK — codename **Helikon**. A Rust SDK for building AI agents with pluggable providers, runtimes, and tools."
3. **Badges row** — three badges shipped now, two stubbed via `<!-- TODO -->` comments:
   - CI status: `https://github.com/SMK1085/paigasus-helikon/actions/workflows/ci.yml/badge.svg?branch=main`
   - MSRV: `https://img.shields.io/badge/rust-1.75%2B-orange.svg` (hand-painted; no API to query workspace MSRV)
   - License: `https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue.svg`
   - `<!-- TODO(post-publish): crates.io badge -->`
   - `<!-- TODO(post-publish): docs.rs badge -->`
4. **One-paragraph pitch** — what the SDK is, who it's for, what it isn't (no opinions on hosting, deployment, observability stacks).
5. **The codename** (~4 lines) — Mount Helicon → Hippocrene spring → struck by Pegasus's hoof → muses' source of inspiration → naming nod to Paigasus.
6. **Install** — preserve the existing `Cargo.toml` snippet (it's correct) and the existing pre-release status note (also correct).
7. **Workspace at a glance** — short bulleted list of the 13 crates by role:
   - `paigasus-helikon` — facade re-exporting `core` and opt-in sibling crates by feature
   - `paigasus-helikon-core` — type system, traits, runtime-agnostic primitives
   - `paigasus-helikon-cli` — `helikon` and `paigasus-helikon` binaries
   - `paigasus-helikon-macros` — proc-macros (currently empty)
   - `paigasus-helikon-providers-{openai,anthropic}` — LLM provider implementations
   - `paigasus-helikon-runtime-{tokio,axum,temporal,agentcore}` — execution / orchestration runtimes
   - `paigasus-helikon-tools` — tool-calling primitives
   - `paigasus-helikon-mcp` — Model Context Protocol integration
   - `paigasus-helikon-evals` — evaluation harness
8. **Documentation** — link to Notion ["Crate Reference"](https://www.notion.so/355830e8fbaa813c92e8c1aa9985fd3f) with note that an mdBook will replace it once published; link to Linear project `Paigasus Helikon` for tracked work.
9. **Contributing** — one-liner pointing at `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`.
10. **License** — full dual-license phrasing:

    > Licensed under either of:
    >
    > - Apache License, Version 2.0 ([LICENSE-APACHE](./LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
    > - MIT license ([LICENSE-MIT](./LICENSE-MIT) or http://opensource.org/licenses/MIT)
    >
    > at your option.
    >
    > ## Contribution
    >
    > Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

    This is the standard Rust ecosystem boilerplate (rust-lang/rust, tokio-rs/tokio, serde-rs/serde all use the same form).

## 6. `CODE_OF_CONDUCT.md` and `SECURITY.md`

### 6.1 `CODE_OF_CONDUCT.md`

Contributor Covenant 2.1 **verbatim** from https://www.contributor-covenant.org/version/2/1/code_of_conduct/. Only substitution:

- `[INSERT CONTACT METHOD]` → `dev@paigasus.com`

The attribution footer (CC BY 4.0, link to FAQ and translations) is **required** by the Covenant's own license and is kept intact.

### 6.2 `SECURITY.md`

Sections:

1. **Supported Versions** — single-row table while pre-release:

   | Version | Supported |
   |---|---|
   | `0.x` (latest minor) | :white_check_mark: |
   | older `0.x` | :x: |

   Footnote: "Once a 1.x line ships, this table tracks the latest 1.x line and the most recent 0.x for one minor cycle."

2. **Reporting a vulnerability** — exactly one channel:

   > Please open a private security advisory at https://github.com/SMK1085/paigasus-helikon/security/advisories/new.
   >
   > Do **not** file a public GitHub issue or post in any public forum until we've had a chance to investigate and ship a fix. Using GitHub PSA keeps the report off public search engines while we work the fix, and gives us a full audit trail.

3. **What to include** — version (`paigasus-helikon` + rustc), minimal repro, impact estimate, suggested remediation if any.

4. **Process and timing**:
   - Acknowledgement: within **5 business days**.
   - Initial status update: within **14 days** of acknowledgement.
   - Coordinated disclosure target: **90 days** from initial report (extendable for high-complexity issues).
   - On fix: a CVE is requested via GitHub Security Advisories, and a RustSec advisory is filed for downstream consumers.

5. **Out of scope** — items not handled via this channel:
   - Denial-of-service via malformed prompts to upstream LLM providers (those are provider-side concerns; report to the provider).
   - Supply-chain advisories already tracked by `cargo audit` — see the daily `scheduled-audit` job in `.github/workflows/audit.yml`. Open a regular issue (with `area:security`) if you want to discuss a published advisory.

## 7. Issue and PR templates

### 7.1 `.github/ISSUE_TEMPLATE/bug_report.yml`

```yaml
name: Bug report
description: Report something that does not work as expected.
title: "bug: "
labels: ["type:bug", "status:triage"]
body:
  - type: markdown
    attributes:
      value: |
        Thanks for taking the time to file a bug report. The more detail you can give, the faster we can help.
  - type: textarea
    id: description
    attributes:
      label: Description
      description: A clear, concise description of the bug.
    validations:
      required: true
  - type: textarea
    id: repro
    attributes:
      label: Reproduction steps
      description: Minimal steps (and a code snippet if possible) to reproduce.
      placeholder: |
        1. ...
        2. ...
        3. ...
    validations:
      required: true
  - type: textarea
    id: expected
    attributes:
      label: Expected behavior
    validations:
      required: true
  - type: textarea
    id: actual
    attributes:
      label: Actual behavior
    validations:
      required: true
  - type: input
    id: paigasus-version
    attributes:
      label: paigasus-helikon version
      placeholder: "0.0.0 / commit abc1234"
    validations:
      required: true
  - type: input
    id: rustc-version
    attributes:
      label: rustc version
      placeholder: "rustc 1.79.0 (stable)"
    validations:
      required: true
  - type: dropdown
    id: os
    attributes:
      label: Operating system
      options:
        - Linux
        - macOS
        - Windows
        - Other (specify in additional context)
    validations:
      required: true
  - type: textarea
    id: context
    attributes:
      label: Additional context
```

### 7.2 `.github/ISSUE_TEMPLATE/feature_request.yml`

```yaml
name: Feature request
description: Suggest a new feature or enhancement.
title: "feat: "
labels: ["type:enhancement", "status:triage"]
body:
  - type: textarea
    id: problem
    attributes:
      label: Problem
      description: What problem are you trying to solve? Who is affected?
    validations:
      required: true
  - type: textarea
    id: solution
    attributes:
      label: Proposed solution
    validations:
      required: true
  - type: textarea
    id: alternatives
    attributes:
      label: Alternatives considered
  - type: textarea
    id: context
    attributes:
      label: Additional context
```

### 7.3 `.github/ISSUE_TEMPLATE/config.yml`

```yaml
blank_issues_enabled: false
contact_links:
  - name: Tracked work (Linear)
    url: https://linear.app/smaschek/project/paigasus-helikon
    about: For roadmap items, internally-tracked work, or anything labeled SMA-###, use Linear.
  - name: Security vulnerabilities
    url: https://github.com/SMK1085/paigasus-helikon/security/advisories/new
    about: Report security issues privately via GitHub Security Advisories — do not file a public issue.
```

### 7.4 `.github/PULL_REQUEST_TEMPLATE.md`

```markdown
## Summary

<!-- 1–3 lines describing what changes and why. -->

## Linked Linear issue

<!-- e.g. Closes SMA-###. Linear auto-closes on merge regardless of the keyword. -->

## Type of change

<!--
The PR title must follow Conventional Commits (e.g. `feat(scope): SMA-### message`).
CI (`pr-title`, `commits`) will reject otherwise. See CONTRIBUTING.md.
-->

## Checklist

- [ ] Tests added or updated
- [ ] Docs updated (`README.md`, `CLAUDE.md`, rustdoc, design doc under `docs/superpowers/specs/` if applicable)
- [ ] `cargo fmt --all` and `cargo clippy --workspace --all-features --all-targets -- -D warnings` clean locally
- [ ] PR title follows Conventional Commits
```

## 8. `CONTRIBUTING.md` delta

Three surgical changes, no restructuring:

1. **Code of Conduct subsection** — three lines near the top of the file (before or alongside the existing "How to contribute" introduction):

   > ### Code of Conduct
   >
   > This project follows the [Contributor Covenant 2.1](./CODE_OF_CONDUCT.md). By participating, you agree to its terms. Report unacceptable behavior to `dev@paigasus.com`.

2. **Security reporting subsection** — under whatever section currently discusses bug reports (or as its own short section if none fits):

   > ### Reporting security issues
   >
   > Please do not file public issues for vulnerabilities. See [SECURITY.md](./SECURITY.md) for the private reporting channel.

3. **License phrasing** — verified at design time: the current `CONTRIBUTING.md` does **not** mention the project's own license (only the `cargo-deny` license allowlist for dependencies, which is unaffected). No edit required here. If the implementer adds new license-mentioning prose for any reason, it must say "dual-licensed under Apache-2.0 OR MIT (see `LICENSE-APACHE` and `LICENSE-MIT`)".

## 9. Verification

### 9.1 Local checks before push

| Command | Why |
|---|---|
| `cargo build --workspace --all-features` | Sanity check that the `Cargo.toml` license string change parses. |
| `cargo deny check licenses` | Confirms the dual-license switch doesn't trip the workspace's own license allowlist. |
| `cargo fmt --all -- --check` | CI gate. |
| Visual render of `README.md` (e.g. `gh markdown-preview` or push and check on GitHub) | "README renders correctly" acceptance criterion. |
| Click every link in the rendered README, CONTRIBUTING.md, CODE_OF_CONDUCT.md, SECURITY.md | "Working links" acceptance criterion. |

### 9.2 CI gates that matter

The full PR will exercise the standard required-status-check set: `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`. The interesting gates for this PR are:

- **`deny`** — license-allowlist smoketest after the relicense.
- **`commits`** + **`pr-title`** — Conventional Commits enforcement on both the per-commit messages and the squash title.

`test` matrix variants on macOS / Windows / 1.75 run as non-required signals.

### 9.3 GitHub community profile

After merge, https://github.com/SMK1085/paigasus-helikon/community should show 100% on:

- [x] Description (already set)
- [x] README
- [x] Code of conduct
- [x] Contributing
- [x] License
- [x] Security policy
- [x] Issue templates
- [x] Pull request template

`gh api repos/SMK1085/paigasus-helikon/community/profile | jq '.health_percentage'` should return `100`.

## 10. Commit and PR strategy

### 10.1 Branch

`feature/sma-310-repo-hygiene-readme-license-contributing-templates` — from Linear's `gitBranchName` field.

### 10.2 Per-commit sequence on the branch

Four atomic commits, each passing CI on its own as much as practical:

1. `chore(license): SMA-310 dual-license under Apache-2.0 OR MIT`
   - `git mv LICENSE LICENSE-MIT`
   - new `LICENSE-APACHE`
   - root `Cargo.toml` workspace license
   - `CLAUDE.md` rule replacement
2. `docs(readme): SMA-310 expand README with codename story, badges, links`
3. `docs(repo): SMA-310 add CODE_OF_CONDUCT.md, SECURITY.md, CONTRIBUTING.md pointers`
4. `chore(github): SMA-310 add issue and PR templates`

The branch is squash-merged. The PR title (= squashed commit message subject) is:

> `docs(repo): SMA-310 baseline OSS hygiene (README, dual-license, CoC, security, templates)`

### 10.3 PR body

Standard Conventional-Commits-flavored body, plus a callout that this PR reverses the MIT-only decision documented in CLAUDE.md (so reviewers and future archaeologists don't have to dig). Includes a checklist mapping each acceptance criterion (§11) to its evidence.

## 11. Acceptance criteria

From the Linear ticket, mapped to evidence in this design:

| Criterion | Evidence |
|---|---|
| `README.md` renders correctly with working links | §5 outline, §9.1 visual render + click-through check |
| `LICENSE-APACHE` and `LICENSE-MIT` present, dual-license in workspace metadata | §3, §4 |
| `CONTRIBUTING.md` covers Conventional Commits, branching, tests/lints, releases, security pointer | Pre-existing in current file + §8 delta |
| `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1) | §6.1 |
| `SECURITY.md` describes reporting flow | §6.2 |
| `.github/ISSUE_TEMPLATE/bug_report.yml`, `feature_request.yml`, `config.yml` (blank disabled) | §7.1, §7.2, §7.3 |
| `.github/PULL_REQUEST_TEMPLATE.md` | §7.4 |
| `gh repo view` / community profile shows healthy state | §9.3 |

## 12. Out of scope

Deliberately not in this ticket:

- `.github/FUNDING.yml` — no funding setup in place yet.
- `.github/SUPPORT.md` — Linear + issue templates cover support routing.
- mdBook for documentation — separate ticket; README links to the Notion reference in the interim.
- Updating the SMA-310 Linear ticket text to reflect the dual-license decision — done out-of-band as a Linear edit, not via this PR.
- Renaming the existing `LICENSE` to `LICENSE-MIT` and adding `LICENSE-APACHE` as separate PRs — single PR per §2 sequencing decision.

## 13. Risks and open questions

| Item | Mitigation |
|---|---|
| Reversing the MIT-only decision so soon after making it may confuse future readers of the git log. | The `CLAUDE.md` replacement text explicitly calls out the reversal date and reasoning; the PR description does the same. |
| The Linear ticket text still says "dual-license" — once we land this, the ticket's text and the actual repo align, but the ticket also contains the now-outdated phrase "the de facto Rust pattern" framing that was true when filed. | Edit the Linear ticket description after merge to note the dual-license decision was carried out (low-priority cosmetic cleanup). |
| `dev@paigasus.com` is a project-scoped alias — confirm it actually routes to a monitored inbox before publishing the CoC. | Verified out-of-band (user-supplied during brainstorming). If routing is broken, the CoC is the wrong place to discover that. |
