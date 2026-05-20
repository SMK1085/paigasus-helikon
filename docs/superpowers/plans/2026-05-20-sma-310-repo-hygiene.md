# SMA-310 — Repo Hygiene Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land baseline open-source hygiene (README polish, dual-license switch, CoC, security policy, issue/PR templates) in a single squash-merged PR on the SMA-310 feature branch.

**Architecture:** Single feature branch, four atomic commits per the spec's §10.2 sequencing. The licensing reversal (MIT-only → Apache-2.0 OR MIT) lands in the same PR because every other doc added by this ticket references the license. Two verbatim third-party documents (Apache-2.0 license text, Contributor Covenant 2.1) are fetched via `curl` rather than inlined — this keeps the plan small enough to write through the Anthropic API and keeps the canonical source authoritative.

**Tech Stack:** Markdown, GitHub issue-form YAML, root `Cargo.toml` workspace metadata. No code changes; no tests beyond verification commands (`cargo build`, `cargo deny`, link checks, community-profile check).

**Branch:** `feature/sma-310-repo-hygiene-readme-license-contributing-templates` (already checked out; spec already committed there as `b65c363`).

**Spec:** `docs/superpowers/specs/2026-05-20-sma-310-repo-hygiene-design.md`.

---

## Pre-flight

- [ ] **Step 0.1: Confirm branch and clean working tree**

Run:
```bash
git rev-parse --abbrev-ref HEAD
git status --short
```

Expected:
- Branch: `feature/sma-310-repo-hygiene-readme-license-contributing-templates`
- Status: clean (or only the spec already committed as `b65c363`).

If the branch is wrong, run `git checkout feature/sma-310-repo-hygiene-readme-license-contributing-templates`. If the working tree is dirty with unrelated changes, stop and ask the user.

- [ ] **Step 0.2: Confirm spec commit is present**

Run:
```bash
git log --oneline -1 -- docs/superpowers/specs/2026-05-20-sma-310-repo-hygiene-design.md
```

Expected: one line showing `b65c363 docs(specs): SMA-310 add design for repo hygiene baseline` (or whichever SHA the spec landed at — the point is the file is tracked).

---

## Task 1: Dual-license switch

**Goal commit:** `chore(license): SMA-310 dual-license under Apache-2.0 OR MIT`

**Files:**
- Rename: `LICENSE` → `LICENSE-MIT` (via `git mv`)
- Create: `LICENSE-APACHE`
- Modify: `Cargo.toml` (root, `[workspace.package]`)
- Modify: `CLAUDE.md` (replace the MIT-only rule)

- [ ] **Step 1.1: Rename `LICENSE` to `LICENSE-MIT`**

Run:
```bash
git mv LICENSE LICENSE-MIT
```

Expected: `LICENSE-MIT` now exists, `LICENSE` is gone, git tracks this as a rename (not delete+add). Verify with:
```bash
git status --short
```

Expected output line: `R  LICENSE -> LICENSE-MIT`.

- [ ] **Step 1.2: Create `LICENSE-APACHE` with the canonical Apache-2.0 text**

Run:
```bash
curl -sSL https://www.apache.org/licenses/LICENSE-2.0.txt -o LICENSE-APACHE
```

Verify the file has the expected SHA-256 (Apache publishes the canonical text; this checksum is stable):
```bash
shasum -a 256 LICENSE-APACHE
```

Expected: a SHA-256 hash. (We don't pin to a specific hash because Apache occasionally updates whitespace; what matters is the file starts with `                                 Apache License` and ends with the standard appendix.)

Verify the header line:
```bash
head -2 LICENSE-APACHE
```

Expected output:
```

                                 Apache License
```

(First line is blank, second line is the title — the canonical text has leading whitespace for indentation.)

- [ ] **Step 1.3: Append the project copyright notice to `LICENSE-APACHE`**

The canonical Apache-2.0 text ships with an "APPENDIX: How to apply" block but no project-specific copyright line. The Rust convention is to append the copyright after the appendix.

Append to `LICENSE-APACHE`:
```
   Copyright 2026 Sven Maschek

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.
```

(Three-space indent matches the canonical Apache appendix's indentation.)

Run:
```bash
tail -15 LICENSE-APACHE
```

Expected: the last 15 lines show the copyright block ending with `limitations under the License.`.

- [ ] **Step 1.4: Update root `Cargo.toml` license field**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/Cargo.toml`, find the `[workspace.package]` section and change the license line.

From:
```toml
license       = "MIT"
```

To:
```toml
license       = "Apache-2.0 OR MIT"
```

(Preserve the existing alignment whitespace before `=`.)

Verify:
```bash
grep -n 'license' Cargo.toml | head -5
```

Expected: a line showing `license       = "Apache-2.0 OR MIT"`.

- [ ] **Step 1.5: Update `CLAUDE.md` — replace the MIT-only rule**

In `/Users/smaschek/dev/paigasus/paigasus-helikon/CLAUDE.md`, find the bullet that begins with `**License is MIT only**`. The current text reads:

```
- **License is MIT only** (decided 2026-05-16). Don't add `LICENSE-APACHE` or set `license = "Apache-2.0 OR MIT"` even though the Cargo ecosystem convention is dual-licensing.
```

Replace the entire bullet with:

```
- **License is dual `Apache-2.0 OR MIT`** (decided 2026-05-20, reversing the 2026-05-16 MIT-only call). Both `LICENSE-APACHE` and `LICENSE-MIT` live at the repo root; the workspace metadata is `license = "Apache-2.0 OR MIT"`. Per Rust ecosystem convention — no Apache-only or MIT-only crates in the workspace. Contributions are accepted under the same dual license by default (the standard inbound-equals-outbound clause is restated in `README.md`).
```

Verify:
```bash
grep -n 'Apache-2.0 OR MIT' CLAUDE.md
```

Expected: at least one match in the "Non-obvious patterns to preserve" section.

Confirm the old wording is gone:
```bash
grep -n 'License is MIT only' CLAUDE.md
```

Expected: no matches.

- [ ] **Step 1.6: Verify the workspace still builds and `deny` is green**

Run:
```bash
cargo build --workspace --all-features
```

Expected: clean build (or unchanged warnings from previous builds — no new errors). If `cargo-deny` is installed:
```bash
cargo deny check licenses
```

Expected: `licenses ok` (the dependency allowlist already permits both `Apache-2.0` and `MIT`).

If `cargo-deny` is not installed locally, skip this — CI's `deny` job will run it.

- [ ] **Step 1.7: Stage and commit**

Run:
```bash
git add LICENSE-MIT LICENSE-APACHE Cargo.toml CLAUDE.md
git status --short
```

Expected output:
```
R  LICENSE -> LICENSE-MIT
A  LICENSE-APACHE
M  Cargo.toml
M  CLAUDE.md
```

Commit:
```bash
git commit -m "chore(license): SMA-310 dual-license under Apache-2.0 OR MIT

Reverses the 2026-05-16 MIT-only decision in favor of the Rust ecosystem
standard dual license. CLAUDE.md updated to reflect the new rule. The
workspace metadata change propagates to every crate via license.workspace
= true; no per-crate Cargo.toml edits required.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

Expected: commit succeeds. If the `commit-msg` hook (cargo-husky) is installed, it validates the Conventional Commits prefix; `chore(license)` is in the allowlist per SMA-335.

---

## Task 2: README expansion

**Goal commit:** `docs(readme): SMA-310 expand README with codename story, badges, links`

**Files:**
- Modify: `README.md` (full rewrite from 18 lines → ~80 lines)

- [ ] **Step 2.1: Rewrite `README.md`**

Replace the entire contents of `/Users/smaschek/dev/paigasus/paigasus-helikon/README.md` with:

````markdown
# paigasus-helikon

Paigasus AI SDK — codename **Helikon**. A Rust SDK for building AI agents with pluggable providers, runtimes, and tools.

[![CI](https://github.com/SMK1085/paigasus-helikon/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/SMK1085/paigasus-helikon/actions/workflows/ci.yml)
[![MSRV](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue.svg)](#license)
<!-- TODO(post-publish): add crates.io badge once the workspace is published -->
<!-- TODO(post-publish): add docs.rs badge once the workspace is published -->

## What it is

`paigasus-helikon` is a Rust SDK for building agentic AI systems. It separates the slow-moving primitives (types, traits, message protocols) from the fast-moving parts (provider SDKs, execution runtimes, tool catalogs), so downstream projects can pick the surface they need without dragging in the rest.

The SDK does not pick a deployment story, a hosting story, or an observability stack for you. Bring your own.

## The codename

In Greek myth, Mount Helicon (Greek: Ἑλικών, *Helikōn*) is the home of the Muses. When Pegasus struck the mountainside with his hoof, the **Hippocrene** spring burst forth — the literal source of poetic inspiration that the Muses drew from.

Paigasus is the umbrella; Helikon is the spring. The SDK is the artifact you draw from when building agents on top.

## Install

```toml
[dependencies]
paigasus-helikon = { version = "0.1", features = ["openai", "anthropic", "mcp", "runtime-tokio"] }
```

> Pre-release: the workspace currently pins `version = "0.0.0"` and is not yet published to crates.io. The `"0.1"` shown above is the planned first published release — replace with the actual published version once available.

## Workspace at a glance

Thirteen crates under `crates/`:

- **`paigasus-helikon`** — facade re-exporting `core` plus opt-in sibling crates by feature flag.
- **`paigasus-helikon-core`** — type system, traits, runtime-agnostic primitives.
- **`paigasus-helikon-cli`** — `helikon` and `paigasus-helikon` binaries.
- **`paigasus-helikon-macros`** — proc-macro crate (currently empty).
- **`paigasus-helikon-providers-openai`**, **`-anthropic`** — LLM provider implementations.
- **`paigasus-helikon-runtime-tokio`**, **`-axum`**, **`-temporal`**, **`-agentcore`** — execution / orchestration runtimes.
- **`paigasus-helikon-tools`** — tool-calling primitives.
- **`paigasus-helikon-mcp`** — Model Context Protocol integration.
- **`paigasus-helikon-evals`** — evaluation harness.

## Documentation

The architectural reference lives in Notion: ["Crate Reference"](https://www.notion.so/355830e8fbaa813c92e8c1aa9985fd3f). An mdBook-hosted equivalent will replace the Notion page once published.

Tracked work lives in Linear under the project **Paigasus Helikon** (issues are prefixed `SMA-`).

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for branching, testing, and release workflows. By participating you agree to the [Contributor Covenant Code of Conduct](./CODE_OF_CONDUCT.md). For security disclosures see [SECURITY.md](./SECURITY.md).

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](./LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](./LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
````

- [ ] **Step 2.2: Verify links syntactically**

Run:
```bash
grep -oE '\]\([^)]+\)' README.md | sort -u
```

Expected: a list of link targets including `./CONTRIBUTING.md`, `./CODE_OF_CONDUCT.md`, `./SECURITY.md`, `./LICENSE-APACHE`, `./LICENSE-MIT`, the Notion URL, the badge URLs. Skim for typos.

Run:
```bash
wc -l README.md
```

Expected: between 65 and 90 lines (target ~80 per spec §5).

- [ ] **Step 2.3: Stage and commit**

Run:
```bash
git add README.md
git commit -m "docs(readme): SMA-310 expand README with codename story, badges, links

Replaces the 18-line stub with a landing page: tagline, badges (CI, MSRV,
license), pitch, codename story (Mt Helicon -> Hippocrene), install
snippet, workspace overview, documentation links, contributing/license
sections. Stubs crates.io and docs.rs badges via TODO comments until
the workspace is published.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

Expected: commit succeeds.

---

## Task 3: CODE_OF_CONDUCT.md, SECURITY.md, and CONTRIBUTING.md delta

**Goal commit:** `docs(repo): SMA-310 add CODE_OF_CONDUCT.md, SECURITY.md, CONTRIBUTING.md pointers`

**Files:**
- Create: `CODE_OF_CONDUCT.md`
- Create: `SECURITY.md`
- Modify: `CONTRIBUTING.md` (two additions; one search-and-replace if applicable)

- [ ] **Step 3.1: Fetch Contributor Covenant 2.1 verbatim**

The canonical Markdown source is published by the Contributor Covenant project. Fetch it directly:

```bash
curl -sSL https://www.contributor-covenant.org/version/2/1/code_of_conduct/code_of_conduct.md -o CODE_OF_CONDUCT.md
```

Verify the file was fetched and has the expected structure:
```bash
head -1 CODE_OF_CONDUCT.md
wc -l CODE_OF_CONDUCT.md
```

Expected:
- First line: `# Contributor Covenant Code of Conduct`
- Total lines: between 130 and 150 (Covenant 2.1 is roughly 137 lines of Markdown).

- [ ] **Step 3.2: Substitute the enforcement contact**

The canonical Covenant 2.1 text contains the placeholder `[INSERT CONTACT METHOD]`. Replace it with the project contact.

Run:
```bash
sed -i.bak 's|\[INSERT CONTACT METHOD\]|dev@paigasus.com|g' CODE_OF_CONDUCT.md
rm CODE_OF_CONDUCT.md.bak
```

(The `.bak` backup is the BSD-sed-on-macOS workaround; the explicit `rm` cleans it up.)

Verify:
```bash
grep -n 'dev@paigasus.com' CODE_OF_CONDUCT.md
```

Expected: at least one match (typically in the "Enforcement" section).

Confirm the placeholder is gone:
```bash
grep -n 'INSERT CONTACT METHOD' CODE_OF_CONDUCT.md
```

Expected: no matches.

- [ ] **Step 3.3: Verify the attribution footer is intact**

The Contributor Covenant license (CC BY 4.0) requires the attribution footer. It must remain in the file.

Run:
```bash
grep -ni 'contributor covenant' CODE_OF_CONDUCT.md | tail -3
```

Expected: at least two lines mentioning "Contributor Covenant" — typically the title at the top and the attribution near the bottom (usually links to https://www.contributor-covenant.org).

If the attribution is missing, the upstream fetch must have served a stripped variant. Stop and investigate before continuing.

- [ ] **Step 3.4: Create `SECURITY.md`**

Create `/Users/smaschek/dev/paigasus/paigasus-helikon/SECURITY.md` with this content:

````markdown
# Security Policy

## Supported Versions

| Version             | Supported          |
| ------------------- | ------------------ |
| `0.x` (latest minor)| :white_check_mark: |
| older `0.x`         | :x:                |

Once a `1.x` line ships, this table will track the latest `1.x` line and the most recent `0.x` for one minor cycle.

## Reporting a Vulnerability

Please open a private security advisory at <https://github.com/SMK1085/paigasus-helikon/security/advisories/new>.

Do **not** file a public GitHub issue or post in any public forum until we have had a chance to investigate and ship a fix. Using GitHub Private Security Advisories keeps the report off public search engines while we work the fix, and gives us a full audit trail.

### What to include

- The version of `paigasus-helikon` (or the commit SHA) you were running.
- The version of `rustc` you were running.
- A minimal reproduction (a snippet, a test, or a description of the failing operation).
- Your estimate of the impact.
- A suggested remediation, if any.

### Process and timing

- **Acknowledgement:** within 5 business days of report.
- **Initial status update:** within 14 days of acknowledgement.
- **Coordinated disclosure target:** 90 days from the initial report. Complex issues may extend this window by mutual agreement.
- **On fix:** we request a CVE through GitHub Security Advisories and file a [RustSec](https://rustsec.org/) advisory so downstream consumers pick it up via `cargo audit`.

## Out of scope

The following are not handled through this channel:

- Denial-of-service via malformed prompts to upstream LLM providers — those are the provider's responsibility, not the SDK's. Report directly to the provider.
- Supply-chain advisories already tracked by `cargo audit`. The repository runs `cargo audit` daily via `.github/workflows/audit.yml` and auto-files issues for new advisories. If you want to discuss an already-published advisory, open a regular issue with the `area:security` label.
````

Verify:
```bash
wc -l SECURITY.md
grep -c 'security/advisories/new' SECURITY.md
```

Expected: ~35 lines; exactly 1 match for `security/advisories/new`.

- [ ] **Step 3.5: Add the Code of Conduct pointer to `CONTRIBUTING.md`**

Open `CONTRIBUTING.md`. Find the very first heading (the file's title, likely `# Contributing to paigasus-helikon` near line 1). Immediately **after** the file's introductory paragraph (and before the next `##` heading), insert this block:

```markdown
## Code of Conduct

This project follows the [Contributor Covenant 2.1](./CODE_OF_CONDUCT.md). By participating, you agree to its terms. Report unacceptable behavior to `dev@paigasus.com`.

```

(Leave a blank line after the block.)

Determine the insertion point first by reading the top of the file:
```bash
head -16 CONTRIBUTING.md
```

This reveals the file's structure. Insert the new `## Code of Conduct` section directly before the first existing `## ` heading.

Verify the change:
```bash
grep -n 'Code of Conduct' CONTRIBUTING.md
```

Expected: at least one match. The section should appear near the top.

- [ ] **Step 3.6: Add the security reporting pointer to `CONTRIBUTING.md`**

Find a natural place for a security pointer — either near the issue-reporting section (if one exists) or as its own short section near the end, before any "License" section. Add this block:

```markdown
## Reporting security issues

Please do not file public issues for vulnerabilities. See [SECURITY.md](./SECURITY.md) for the private reporting channel.

```

If `CONTRIBUTING.md` is mostly process documentation (commits, tests, releases) and has no obvious "reporting bugs" section, place this block immediately before the existing "Repo configuration" section (the file's natural penultimate block, per SMA-309).

Verify:
```bash
grep -n 'Reporting security' CONTRIBUTING.md
grep -n 'SECURITY.md' CONTRIBUTING.md
```

Expected: at least one match for each.

- [ ] **Step 3.7: Verify no stale "MIT-only" wording remains in `CONTRIBUTING.md`**

Per spec §8 item 3, this was verified at design time: `CONTRIBUTING.md` does not currently mention the project's own license (only the `cargo-deny` dependency allowlist). Re-confirm:

```bash
grep -niE 'mit[- ]?licen' CONTRIBUTING.md
```

Expected: no matches. If a match appears, edit the line to read "dual-licensed under Apache-2.0 OR MIT (see `LICENSE-APACHE` and `LICENSE-MIT`)". If no match appears, no edit is needed.

- [ ] **Step 3.8: Stage and commit**

Run:
```bash
git add CODE_OF_CONDUCT.md SECURITY.md CONTRIBUTING.md
git status --short
```

Expected output:
```
A  CODE_OF_CONDUCT.md
M  CONTRIBUTING.md
A  SECURITY.md
```

Commit:
```bash
git commit -m "docs(repo): SMA-310 add Code of Conduct, security policy, and CONTRIBUTING pointers

Adds Contributor Covenant 2.1 verbatim with dev@paigasus.com as the
enforcement contact. Adds SECURITY.md with a GitHub Private Security
Advisories-only reporting flow, supported-versions table, and 90-day
coordinated-disclosure target. CONTRIBUTING.md gains a Code of Conduct
section near the top and a security pointer near the end.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

Expected: commit succeeds.

---

## Task 4: Issue and PR templates

**Goal commit:** `chore(github): SMA-310 add issue and PR templates`

**Files:**
- Create: `.github/ISSUE_TEMPLATE/bug_report.yml`
- Create: `.github/ISSUE_TEMPLATE/feature_request.yml`
- Create: `.github/ISSUE_TEMPLATE/config.yml`
- Create: `.github/PULL_REQUEST_TEMPLATE.md`

- [ ] **Step 4.1: Create the ISSUE_TEMPLATE directory**

Run:
```bash
mkdir -p .github/ISSUE_TEMPLATE
ls -la .github/ISSUE_TEMPLATE
```

Expected: directory exists, empty (or with no conflicting files).

- [ ] **Step 4.2: Create `.github/ISSUE_TEMPLATE/bug_report.yml`**

Create the file with this exact content:

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

- [ ] **Step 4.3: Create `.github/ISSUE_TEMPLATE/feature_request.yml`**

Create the file with this exact content:

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

- [ ] **Step 4.4: Create `.github/ISSUE_TEMPLATE/config.yml`**

Create the file with this exact content:

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

- [ ] **Step 4.5: Create `.github/PULL_REQUEST_TEMPLATE.md`**

Create the file with this exact content:

```markdown
## Summary

<!-- 1-3 lines describing what changes and why. -->

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

- [ ] **Step 4.6: Verify YAML validity for the three issue templates**

If `yq` is installed:
```bash
yq eval . .github/ISSUE_TEMPLATE/bug_report.yml > /dev/null && echo "bug_report.yml OK"
yq eval . .github/ISSUE_TEMPLATE/feature_request.yml > /dev/null && echo "feature_request.yml OK"
yq eval . .github/ISSUE_TEMPLATE/config.yml > /dev/null && echo "config.yml OK"
```

Expected: three `OK` lines.

If `yq` is not installed, GitHub validates issue forms server-side after push; CI will not fail on syntactically broken forms, but the templates will silently not appear in the "New issue" picker. Inspect each file visually instead:
```bash
cat .github/ISSUE_TEMPLATE/bug_report.yml | head -5
cat .github/ISSUE_TEMPLATE/feature_request.yml | head -5
cat .github/ISSUE_TEMPLATE/config.yml | head -5
```

Expected: `name:`/`description:`/`title:` keys at the top of each form file; `blank_issues_enabled: false` at the top of `config.yml`.

- [ ] **Step 4.7: Stage and commit**

Run:
```bash
git add .github/ISSUE_TEMPLATE/ .github/PULL_REQUEST_TEMPLATE.md
git status --short
```

Expected output:
```
A  .github/ISSUE_TEMPLATE/bug_report.yml
A  .github/ISSUE_TEMPLATE/config.yml
A  .github/ISSUE_TEMPLATE/feature_request.yml
A  .github/PULL_REQUEST_TEMPLATE.md
```

Commit:
```bash
git commit -m "chore(github): SMA-310 add issue and PR templates

Adds three issue templates (bug_report.yml, feature_request.yml,
config.yml with blank_issues_enabled: false) and a lean PR template.
The config.yml contact_links route tracked work to Linear and security
reports to GitHub Private Security Advisories.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

Expected: commit succeeds.

---

## Task 5: Push, open PR, verify

**Goal:** Get the PR open with a Conventional Commits-compliant title, all CI gates green.

- [ ] **Step 5.1: Run local CI gates one more time before push**

Run these in order. Each must be clean before proceeding:

```bash
cargo fmt --all -- --check
```
Expected: no output (clean) and exit code 0.

```bash
cargo clippy --workspace --all-features --all-targets -- -D warnings
```
Expected: clean build, no warnings escalated to errors. (This is a docs-only PR, so clippy should have no new findings to report on the changed files. If it does, investigate — there may be a `Cargo.toml` syntax issue introduced in Task 1.)

```bash
cargo test --workspace --all-features
```
Expected: all tests pass. (No tests were added; this is a regression smoketest only.)

If `cargo-deny` is installed:
```bash
cargo deny check
```
Expected: `advisories ok`, `bans ok`, `licenses ok`, `sources ok`.

- [ ] **Step 5.2: Review the branch's commit log**

Run:
```bash
git log --oneline main..HEAD
```

Expected (most recent first): five commits — the four task commits plus the spec commit from before plan-writing:
```
<sha> chore(github): SMA-310 add issue and PR templates
<sha> docs(repo): SMA-310 add Code of Conduct, security policy, and CONTRIBUTING pointers
<sha> docs(readme): SMA-310 expand README with codename story, badges, links
<sha> chore(license): SMA-310 dual-license under Apache-2.0 OR MIT
b65c363 docs(specs): SMA-310 add design for repo hygiene baseline
```

Confirm each commit message starts with a Conventional Commits prefix and includes `SMA-310`. If the `commit-msg` hook is installed locally, this is already guaranteed; the manual check is a safety net.

- [ ] **Step 5.3: Push the branch**

Run:
```bash
git push -u origin feature/sma-310-repo-hygiene-readme-license-contributing-templates
```

Expected: push succeeds, tracking is set to `origin/feature/sma-310-...`. Watch for branch-name-ruleset rejections — the SMA-309 ruleset permits this branch-name pattern, so it should pass.

- [ ] **Step 5.4: Open the PR**

Run:
```bash
gh pr create \
  --title "docs(repo): SMA-310 baseline OSS hygiene (README, dual-license, CoC, security, templates)" \
  --body "$(cat <<'EOF'
## Summary

Lands the OSS hygiene baseline so external contributors can submit work against `paigasus-helikon`:

- README rewritten with codename story, badges, install, links, dual-license phrasing.
- License switched to **Apache-2.0 OR MIT** (reverses the 2026-05-16 MIT-only decision). `LICENSE` renamed to `LICENSE-MIT`; `LICENSE-APACHE` added; root `Cargo.toml` workspace metadata updated. `CLAUDE.md` rule replaced to reflect the new policy.
- `CODE_OF_CONDUCT.md` — Contributor Covenant 2.1 verbatim with `dev@paigasus.com` as the enforcement contact.
- `SECURITY.md` — GitHub PSA-only reporting flow with 90-day coordinated disclosure target.
- `CONTRIBUTING.md` — Code of Conduct and security-reporting pointers added (no restructuring).
- `.github/ISSUE_TEMPLATE/{bug_report.yml,feature_request.yml,config.yml}` — three GitHub issue forms; blank issues disabled; Linear and PSA contact links surfaced.
- `.github/PULL_REQUEST_TEMPLATE.md` — lean ~25-line template with Conventional Commits guidance.

## Linked Linear issue

Closes SMA-310. (Linear auto-closes on merge regardless.)

## Notes for reviewers

This PR reverses the **MIT-only** licensing decision documented in `CLAUDE.md` on 2026-05-16. The reversal is explicit and intentional — the new rule is dual-licensing under `Apache-2.0 OR MIT`, the Rust ecosystem convention. The `CLAUDE.md` edit replaces (rather than removes) the relevant bullet so the convention remains discoverable.

After merge, `https://github.com/SMK1085/paigasus-helikon/community` should show 100% community health.

## Test plan

- [ ] CI green on all required gates (`fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`).
- [ ] Render README on GitHub and confirm every link resolves (Notion, Linear, license files, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`).
- [ ] Open the "New issue" picker on the PR's branch (or after merge) and confirm `bug_report` and `feature_request` forms appear; "Open a blank issue" link is absent.
- [ ] `gh api repos/SMK1085/paigasus-helikon/community/profile | jq '.health_percentage'` returns `100` (run after merge to `main`).

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: `gh pr create` returns the PR URL. Capture it for the next steps.

- [ ] **Step 5.5: Wait for CI**

Watch the PR's checks:
```bash
gh pr checks --watch
```

Expected: all required gates eventually report `pass`:
- `fmt`
- `clippy`
- `test (ubuntu-latest, stable)`
- `docs`
- `doc-coverage`
- `commits`
- `pr-title`
- `audit`
- `deny`

If any required gate fails, fix the underlying issue, create a new commit on the branch (do not `--amend`), push, and re-watch.

If `pr-title` fails with a parser error on `docs(repo)`, check `.versionrc` — `docs` is in the allowlist per SMA-335, so this should pass. The most likely failure mode is forgetting `SMA-310` in the title (not applicable here) or a typo in the type.

- [ ] **Step 5.6: Verify links manually on the rendered PR**

Open the PR in a browser. Open the "Files changed" tab and the rendered preview of `README.md`. Click each link:

- CI badge → GitHub Actions page for `ci.yml`.
- License badge → anchor link to `#license` section in the README.
- Notion "Crate Reference" link → loads the Notion page.
- Linear project link → loads the Linear project board.
- `./LICENSE-APACHE`, `./LICENSE-MIT`, `./CONTRIBUTING.md`, `./CODE_OF_CONDUCT.md`, `./SECURITY.md` → load the corresponding file on the PR's branch.

If any link is broken, fix it in a new commit on the branch.

- [ ] **Step 5.7: Squash-merge (after approval and green CI)**

When CI is green and (per the SMA-309 ruleset) the maintainer self-approves with the admin bypass:

```bash
gh pr merge --squash --delete-branch
```

The squash commit subject is the PR title (verified by SMA-309's `squash-commit-title = PR_TITLE`). The squashed commit lands on `main` as a single line, parseable by release-plz.

- [ ] **Step 5.8: Verify community profile is at 100%**

After merge, run:
```bash
gh api repos/SMK1085/paigasus-helikon/community/profile | jq '.health_percentage, .files | keys'
```

Expected:
- `health_percentage`: `100`
- `.files` keys (or values) include: `code_of_conduct`, `contributing`, `issue_template`, `license`, `pull_request_template`, `readme`.

If `health_percentage` is below 100, run the same command and inspect `.files` for `null` entries — those are the missing files. Fix in a follow-up PR.

- [ ] **Step 5.9: Verify Linear status**

The PR description's `Closes SMA-310` should have triggered Linear's auto-close (per the memory `feedback_linear_auto_closes_on_merge`).

In the Linear web UI, confirm SMA-310 is now in **Done**. No manual transition required.

---

## Done

All acceptance criteria from spec §11 mapped to evidence:

| Criterion | Where verified |
|---|---|
| `README.md` renders correctly with working links | Steps 2.2, 5.6 |
| `LICENSE-APACHE` and `LICENSE-MIT` present, dual-license in workspace metadata | Steps 1.1–1.4 |
| `CONTRIBUTING.md` covers Conventional Commits, branching, tests/lints, releases, security pointer | Pre-existing + Steps 3.5–3.7 |
| `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1) present | Steps 3.1–3.3 |
| `SECURITY.md` describes reporting flow | Step 3.4 |
| Issue templates (`bug_report.yml`, `feature_request.yml`, `config.yml`) present | Steps 4.2–4.4 |
| `.github/PULL_REQUEST_TEMPLATE.md` present | Step 4.5 |
| `gh repo view` / community profile shows healthy state | Step 5.8 |
