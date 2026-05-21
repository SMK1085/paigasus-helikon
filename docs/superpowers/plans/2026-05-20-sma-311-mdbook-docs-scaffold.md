# SMA-311 mdBook docs scaffold — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up a published mdBook scaffold at <https://smk1085.github.io/paigasus-helikon/>, gated by a `book-build` CI check that runs mdBook + linkcheck on every PR.

**Architecture:** A new `docs/book/` directory sibling to `docs/superpowers/`. Stub Markdown pages, one per chapter in the Notion Architecture hub, all using a uniform "Stub" callout. A two-job workflow `.github/workflows/docs.yml` builds on PRs and deploys to GitHub Pages on push to `main` via the native `actions/deploy-pages` flow. `book-build` is added to the main-branch required status checks.

**Tech Stack:** mdBook 0.4.43, mdbook-linkcheck 0.7.7, GitHub Actions (`taiki-e/install-action`, `actions/upload-pages-artifact`, `actions/deploy-pages`), Bash, Markdown.

**Reference spec:** [`docs/superpowers/specs/2026-05-20-sma-311-mdbook-docs-scaffold-design.md`](../specs/2026-05-20-sma-311-mdbook-docs-scaffold-design.md).

**Working branch:** `feature/sma-311-mdbook-docs-scaffold` (already created and contains the design doc).

---

## File map

**Created:**
- `docs/book/book.toml`
- `docs/book/.gitignore`
- `docs/book/src/SUMMARY.md`
- `docs/book/src/introduction.md`
- `docs/book/src/getting-started/quickstart.md`
- `docs/book/src/getting-started/workspace-layout.md`
- `docs/book/src/concepts/core-primitives.md`
- `docs/book/src/concepts/agent-loop.md`
- `docs/book/src/concepts/tools.md`
- `docs/book/src/concepts/model-providers.md`
- `docs/book/src/concepts/sessions.md`
- `docs/book/src/concepts/multi-agent-patterns.md`
- `docs/book/src/concepts/permissions-guardrails-hooks.md`
- `docs/book/src/concepts/mcp-integration.md`
- `docs/book/src/concepts/observability-evaluation.md`
- `docs/book/src/concepts/structured-output-builder.md`
- `docs/book/src/reference/crates.md`
- `docs/book/src/reference/api-docs.md`
- `docs/book/src/decisions/index.md`
- `.github/workflows/docs.yml`

**Modified:**
- `.github/rulesets/main-protection-checks.json`
- `README.md`
- `CONTRIBUTING.md`

**Commits planned (three, on `feature/sma-311-mdbook-docs-scaffold`):**
1. `docs(repo): SMA-311 scaffold mdBook book (book.toml, SUMMARY, stubs)`
2. `docs(repo): SMA-311 add Pages deploy workflow and required-check entry`
3. `docs(repo): SMA-311 cross-link docs site from README and CONTRIBUTING`

The repo's `.versionrc` enforces an explicit `scopeRegex` allowlist (validated by the local commit-msg hook and the `commits` CI check). `book` is not in that allowlist; `repo` is, and is the SMA-309 / SMA-310 precedent for repo-wide tooling changes. Do not invent a new scope; use `repo`.

---

## Task 1: Verify local toolchain prerequisites

**Files:** none

- [ ] **Step 1: Install mdBook and mdbook-linkcheck at the pinned versions**

Run:

```bash
cargo install mdbook --version 0.4.43 --locked
cargo install mdbook-linkcheck --version 0.7.7 --locked
```

If a newer minor is available (`cargo search mdbook` or check <https://crates.io/crates/mdbook>), use the newer one and update the version pins in `book.toml`-adjacent docs in Tasks 2 and the workflow file in Task 12. Document the actual version installed.

Expected: both binaries install without error.

- [ ] **Step 2: Verify the binaries are on PATH**

Run:

```bash
mdbook --version
mdbook-linkcheck --version
```

Expected: both print version strings (e.g. `mdbook v0.4.43`, `mdbook-linkcheck 0.7.7`).

- [ ] **Step 3: Verify current branch**

Run:

```bash
git branch --show-current
```

Expected: `feature/sma-311-mdbook-docs-scaffold`.

If the branch is `main`, switch with `git checkout feature/sma-311-mdbook-docs-scaffold` (it was created during the design phase).

---

## Task 2: Create `docs/book/book.toml` and `.gitignore`

**Files:**
- Create: `docs/book/book.toml`
- Create: `docs/book/.gitignore`

- [ ] **Step 1: Create the book directory**

Run:

```bash
mkdir -p docs/book/src/getting-started docs/book/src/concepts docs/book/src/reference docs/book/src/decisions
```

Expected: directories created. Verify with `find docs/book -type d`.

- [ ] **Step 2: Write `docs/book/book.toml`**

Create `docs/book/book.toml` with:

```toml
[book]
title       = "Paigasus Helikon"
description = "Public documentation for the Paigasus AI SDK (Rust)."
authors     = ["Sven Maschek"]
language    = "en"
src         = "src"

[output.html]
site-url             = "/paigasus-helikon/"
git-repository-url   = "https://github.com/SMK1085/paigasus-helikon"
edit-url-template    = "https://github.com/SMK1085/paigasus-helikon/edit/main/docs/book/{path}"
default-theme        = "rust"
preferred-dark-theme = "ayu"

[output.html.fold]
enable = true
level  = 1

[output.linkcheck]
follow-web-links = false
warning-policy   = "error"
exclude          = []
```

- [ ] **Step 3: Write `docs/book/.gitignore`**

Create `docs/book/.gitignore` with:

```
book/
```

- [ ] **Step 4: Do not run `mdbook build` yet**

The book has no `SUMMARY.md` at this point; `mdbook build` would fail. Verification happens at the end of Task 7 once all pages exist.

---

## Task 3: Write `SUMMARY.md` and the Introduction page

**Files:**
- Create: `docs/book/src/SUMMARY.md`
- Create: `docs/book/src/introduction.md`

- [ ] **Step 1: Write `docs/book/src/SUMMARY.md`**

Create with this exact content:

```markdown
# Summary

[Introduction](./introduction.md)

# Getting Started

- [Quickstart](./getting-started/quickstart.md)
- [Workspace layout](./getting-started/workspace-layout.md)

# Concepts

- [Core Primitives](./concepts/core-primitives.md)
- [Agent Loop & State Machine](./concepts/agent-loop.md)
- [Tools](./concepts/tools.md)
- [Model Providers](./concepts/model-providers.md)
- [Sessions](./concepts/sessions.md)
- [Multi-Agent Patterns](./concepts/multi-agent-patterns.md)
- [Permissions, Guardrails & Hooks](./concepts/permissions-guardrails-hooks.md)
- [MCP Integration](./concepts/mcp-integration.md)
- [Observability & Evaluation](./concepts/observability-evaluation.md)
- [Structured Output & Builder](./concepts/structured-output-builder.md)

# Reference

- [Crate overview](./reference/crates.md)
- [API docs](./reference/api-docs.md)

# Decisions

- [Index](./decisions/index.md)
```

- [ ] **Step 2: Write `docs/book/src/introduction.md`**

Create with this exact content:

```markdown
# Introduction

`paigasus-helikon` is a Rust SDK for building agentic AI systems. It separates slow-moving primitives (types, traits, message protocols) from fast-moving parts (provider SDKs, execution runtimes, tool catalogs), so downstream projects can pick the surface they need without dragging in the rest.

The SDK does not pick a deployment story, a hosting story, or an observability stack for you. Bring your own.

## What's here

This documentation site is published from the [`paigasus-helikon`](https://github.com/SMK1085/paigasus-helikon) repository. It is currently a **scaffold** — the chapter structure is in place, but most pages are stubs. Real content lands page-by-page alongside the corresponding feature tickets.

## What's not yet here

API documentation lives on [docs.rs](https://docs.rs) once the workspace is published. Internal architectural design notes live in Notion until they migrate here. Tracked work lives in [Linear](https://linear.app/smaschek) under the project **Paigasus Helikon**.
```

---

## Task 4: Write the Getting Started stubs

**Files:**
- Create: `docs/book/src/getting-started/quickstart.md`
- Create: `docs/book/src/getting-started/workspace-layout.md`

- [ ] **Step 1: Write `quickstart.md`**

Create with:

```markdown
# Quickstart

A minimal example showing how to add `paigasus-helikon` to a Rust project and run an agent against a single provider.

> **Stub.** Full content lands with the corresponding implementation ticket. The architectural source-of-truth for this section currently lives in internal design notes; it will move here as the SDK matures.
```

- [ ] **Step 2: Write `workspace-layout.md`**

Create with:

```markdown
# Workspace layout

Overview of the 13-crate workspace: the facade, core, providers, runtimes, tools, MCP integration, evals, and CLI binaries.

> **Stub.** Full content lands with the corresponding implementation ticket. The architectural source-of-truth for this section currently lives in internal design notes; it will move here as the SDK matures.
```

---

## Task 5: Write the Concepts stubs (10 pages)

**Files:** one per page, all under `docs/book/src/concepts/`.

Each page follows the same template: H1, one-sentence intent statement (taken from the Notion Architecture hub), uniform Stub callout. The intent statements are reproduced verbatim below so the executor doesn't need to look anything up.

- [ ] **Step 1: Write `core-primitives.md`**

```markdown
# Core Primitives

The seven traits (`Model`, `Tool`, `Agent`, `Session`, `Guardrail`, `Hook`, `Runner`) and the concrete types they share.

> **Stub.** Full content lands with the corresponding implementation ticket. The architectural source-of-truth for this section currently lives in internal design notes; it will move here as the SDK matures.
```

- [ ] **Step 2: Write `agent-loop.md`**

```markdown
# Agent Loop & State Machine

Why the loop is an explicit `enum LoopState`, and how it drives the typed event stream.

> **Stub.** Full content lands with the corresponding implementation ticket. The architectural source-of-truth for this section currently lives in internal design notes; it will move here as the SDK matures.
```

- [ ] **Step 3: Write `tools.md`**

```markdown
# Tools

The `Tool` trait, the `#[tool]` macro, schemars-derived JSON Schema, and heterogeneous tool registries.

> **Stub.** Full content lands with the corresponding implementation ticket. The architectural source-of-truth for this section currently lives in internal design notes; it will move here as the SDK matures.
```

- [ ] **Step 4: Write `model-providers.md`**

```markdown
# Model Providers

The single `Model` trait, `ModelCapabilities`, provider crates, and the async-trait vs AFIT trade-off.

> **Stub.** Full content lands with the corresponding implementation ticket. The architectural source-of-truth for this section currently lives in internal design notes; it will move here as the SDK matures.
```

- [ ] **Step 5: Write `sessions.md`**

```markdown
# Sessions

Append-only event log, projections, default backends, and the compaction wrapper.

> **Stub.** Full content lands with the corresponding implementation ticket. The architectural source-of-truth for this section currently lives in internal design notes; it will move here as the SDK matures.
```

- [ ] **Step 6: Write `multi-agent-patterns.md`**

```markdown
# Multi-Agent Patterns

`LlmAgent`, Sequential / Parallel / Loop, Swarm, Graph, Agent-as-Tool, and Handoffs.

> **Stub.** Full content lands with the corresponding implementation ticket. The architectural source-of-truth for this section currently lives in internal design notes; it will move here as the SDK matures.
```

- [ ] **Step 7: Write `permissions-guardrails-hooks.md`**

```markdown
# Permissions, Guardrails & Hooks

Permission modes, decisions, guardrail tripwires, and lifecycle hooks.

> **Stub.** Full content lands with the corresponding implementation ticket. The architectural source-of-truth for this section currently lives in internal design notes; it will move here as the SDK matures.
```

- [ ] **Step 8: Write `mcp-integration.md`**

```markdown
# MCP Integration

`rmcp` client wrapper, lazy tool loading, and exposing agents as MCP servers.

> **Stub.** Full content lands with the corresponding implementation ticket. The architectural source-of-truth for this section currently lives in internal design notes; it will move here as the SDK matures.
```

- [ ] **Step 9: Write `observability-evaluation.md`**

```markdown
# Observability & Evaluation

OTel + GenAI conventions; eval hooks (replay, recorded traces, trajectory).

> **Stub.** Full content lands with the corresponding implementation ticket. The architectural source-of-truth for this section currently lives in internal design notes; it will move here as the SDK matures.
```

- [ ] **Step 10: Write `structured-output-builder.md`**

```markdown
# Structured Output & Builder

`output_type::<T>()`, schemars + retry/repair, and the typestate builder.

> **Stub.** Full content lands with the corresponding implementation ticket. The architectural source-of-truth for this section currently lives in internal design notes; it will move here as the SDK matures.
```

---

## Task 6: Write the Reference stubs

**Files:**
- Create: `docs/book/src/reference/crates.md`
- Create: `docs/book/src/reference/api-docs.md`

- [ ] **Step 1: Write `crates.md`**

```markdown
# Crate overview

Summary of the 13-crate workspace: which crate owns which concern, which features pull which crates into the facade, and the dependency direction between core and the sibling crates.

> **Stub.** Full content lands with the corresponding implementation ticket. The architectural source-of-truth for this section currently lives in internal design notes; it will move here as the SDK matures.
```

- [ ] **Step 2: Write `api-docs.md`**

```markdown
# API docs

Per-item Rust API documentation lives on [docs.rs](https://docs.rs) once the workspace is published. Until then, build the docs locally with `cargo doc --workspace --all-features --no-deps --open`.

> **Stub.** Full content lands with the corresponding implementation ticket. The architectural source-of-truth for this section currently lives in internal design notes; it will move here as the SDK matures.
```

---

## Task 7: Write the Decisions index

**Files:**
- Create: `docs/book/src/decisions/index.md`

- [ ] **Step 1: Write `decisions/index.md`**

```markdown
# Decisions

Architectural decisions are currently captured as **design docs alongside their Linear tickets**, stored under [`docs/superpowers/specs/`](https://github.com/SMK1085/paigasus-helikon/tree/main/docs/superpowers/specs) in the repository.

Once the SDK ships its first user-facing release, decisions affecting the public API will graduate to a formal ADR/MADR section here.
```

---

## Task 8: Build the scaffold locally and verify

**Files:** none

- [ ] **Step 1: Run `mdbook build`**

Run:

```bash
cd docs/book && mdbook build && cd -
```

Expected: exit 0. mdBook prints `Running the html backend` and `Running the linkcheck backend`. Linkcheck reports zero broken links. The output appears under `docs/book/book/html/` (the html subdirectory is the linkcheck-induced layout).

If linkcheck reports a broken link, fix the SUMMARY.md path or the offending file path. The 19 markdown files plus SUMMARY.md should resolve cleanly.

- [ ] **Step 2: Inspect the output tree**

Run:

```bash
ls docs/book/book/html/ | head -20
```

Expected: an `index.html`, plus per-page HTML files in `concepts/`, `getting-started/`, `reference/`, `decisions/` subdirectories.

- [ ] **Step 3: Spot-check the local server (optional but recommended)**

Run:

```bash
cd docs/book && mdbook serve --open &
SERVE_PID=$!
sleep 3
curl -fsS http://localhost:3000/ -o /tmp/index.html && head -5 /tmp/index.html
kill $SERVE_PID
cd -
```

Expected: `curl` returns 200; `head` shows the start of the rendered HTML.

(If running interactively, just `cd docs/book && mdbook serve --open` and visually confirm the SUMMARY.md tree is intact, then Ctrl-C.)

- [ ] **Step 4: Verify the build output is gitignored**

Run:

```bash
git status docs/book/book/
```

Expected: no output (the `book/` directory is ignored by `docs/book/.gitignore`).

---

## Task 9: Commit the scaffold

**Files:** none

- [ ] **Step 1: Stage the new files**

Run:

```bash
git add docs/book/
git status --short
```

Expected: 21 new files under `docs/book/` (book.toml, .gitignore, SUMMARY.md, 18 page files in src/). No files under `docs/book/book/` (the build output).

- [ ] **Step 2: Commit**

Run:

```bash
git commit -m "$(cat <<'EOF'
docs(repo): SMA-311 scaffold mdBook book (book.toml, SUMMARY, stubs)

Adds the public documentation site scaffold under docs/book/:
- book.toml with html + linkcheck output backends, site-url for the
  GitHub Pages subpath, and edit-url-template pointing at the repo.
- SUMMARY.md and 18 stub pages organized into Introduction,
  Getting Started, Concepts (10 pages mirroring the Notion
  Architecture hub), Reference, and Decisions sections.
- docs/book/.gitignore to keep the local build output out of git.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds. If a pre-commit hook fails, do not `--amend`; fix the issue, re-stage, and create a new commit.

- [ ] **Step 3: Verify the commit landed**

Run:

```bash
git log -1 --stat
```

Expected: commit shows 21 files added under `docs/book/`.

---

## Task 10: Resolve action SHAs for the workflow

**Files:** none (research step; SHAs are written into the workflow file in Task 11)

The workflow needs SHA pins for four actions, per CLAUDE.md's "implement GitHub Actions against the latest stable major" rule. This task resolves them; Task 11 pastes them in.

- [ ] **Step 1: Resolve `actions/checkout` latest stable**

Run:

```bash
TAG=$(gh api repos/actions/checkout/releases/latest --jq '.tag_name')
REF=$(gh api repos/actions/checkout/git/ref/tags/"$TAG" --jq '.object.sha,.object.type' | paste -sd' ' -)
SHA=$(echo "$REF" | awk '{print $1}')
TYPE=$(echo "$REF" | awk '{print $2}')
if [ "$TYPE" = "tag" ]; then SHA=$(gh api repos/actions/checkout/git/tags/"$SHA" --jq '.object.sha'); fi
echo "actions/checkout $TAG $SHA"
```

Record the printed `<tag> <sha>`. Use the SHA in Task 11; the tag goes in the `# vX.Y.Z` comment.

- [ ] **Step 2: Resolve `taiki-e/install-action` latest stable**

Run the same pattern, substituting the repo:

```bash
TAG=$(gh api repos/taiki-e/install-action/releases/latest --jq '.tag_name')
REF=$(gh api repos/taiki-e/install-action/git/ref/tags/"$TAG" --jq '.object.sha,.object.type' | paste -sd' ' -)
SHA=$(echo "$REF" | awk '{print $1}')
TYPE=$(echo "$REF" | awk '{print $2}')
if [ "$TYPE" = "tag" ]; then SHA=$(gh api repos/taiki-e/install-action/git/tags/"$SHA" --jq '.object.sha'); fi
echo "taiki-e/install-action $TAG $SHA"
```

Record the result.

- [ ] **Step 3: Resolve `actions/upload-pages-artifact` latest stable**

```bash
TAG=$(gh api repos/actions/upload-pages-artifact/releases/latest --jq '.tag_name')
REF=$(gh api repos/actions/upload-pages-artifact/git/ref/tags/"$TAG" --jq '.object.sha,.object.type' | paste -sd' ' -)
SHA=$(echo "$REF" | awk '{print $1}')
TYPE=$(echo "$REF" | awk '{print $2}')
if [ "$TYPE" = "tag" ]; then SHA=$(gh api repos/actions/upload-pages-artifact/git/tags/"$SHA" --jq '.object.sha'); fi
echo "actions/upload-pages-artifact $TAG $SHA"
```

Record the result.

- [ ] **Step 4: Resolve `actions/deploy-pages` latest stable**

```bash
TAG=$(gh api repos/actions/deploy-pages/releases/latest --jq '.tag_name')
REF=$(gh api repos/actions/deploy-pages/git/ref/tags/"$TAG" --jq '.object.sha,.object.type' | paste -sd' ' -)
SHA=$(echo "$REF" | awk '{print $1}')
TYPE=$(echo "$REF" | awk '{print $2}')
if [ "$TYPE" = "tag" ]; then SHA=$(gh api repos/actions/deploy-pages/git/tags/"$SHA" --jq '.object.sha'); fi
echo "actions/deploy-pages $TAG $SHA"
```

Record the result.

- [ ] **Step 5: Sanity-check majors**

Use whatever major version each action's `releases/latest` returns at execution time — this is the "latest stable major" per CLAUDE.md. The pin lives in the workflow as the resolved commit SHA, with the human-readable version in a trailing `# vX.Y.Z` comment. If `releases/latest` returns an older major because nothing in a newer major has shipped yet, use what is returned and note the choice in the PR description.

---

## Task 11: Create `.github/workflows/docs.yml`

**Files:**
- Create: `.github/workflows/docs.yml`

- [ ] **Step 1: Write the workflow file**

Create `.github/workflows/docs.yml` with the following content, substituting `<sha-checkout>`, `<tag-checkout>`, etc. with the values from Task 10. **Do not commit `<sha-...>` placeholders** — every one must be resolved.

```yaml
name: docs

on:
  push:
    branches: [main]
  pull_request:

permissions:
  contents: read

concurrency:
  group: docs-${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

env:
  MDBOOK_VERSION: "0.4.43"
  MDBOOK_LINKCHECK_VERSION: "0.7.7"

jobs:
  book-build:
    runs-on: ubuntu-latest
    steps:
      # actions/checkout <tag-checkout>
      - uses: actions/checkout@<sha-checkout>
      # taiki-e/install-action <tag-install>
      - uses: taiki-e/install-action@<sha-install>
        with:
          tool: mdbook@${{ env.MDBOOK_VERSION }},mdbook-linkcheck@${{ env.MDBOOK_LINKCHECK_VERSION }}
      - name: Build mdBook (HTML + linkcheck)
        working-directory: docs/book
        run: mdbook build
      # actions/upload-pages-artifact <tag-upload>
      - name: Upload Pages artifact
        if: github.ref == 'refs/heads/main' && github.event_name == 'push'
        uses: actions/upload-pages-artifact@<sha-upload>
        with:
          path: docs/book/book/html

  book-deploy:
    needs: book-build
    if: github.ref == 'refs/heads/main' && github.event_name == 'push'
    runs-on: ubuntu-latest
    permissions:
      pages: write
      id-token: write
    environment:
      name: github-pages
      url: ${{ steps.deployment.outputs.page_url }}
    concurrency:
      group: pages
      cancel-in-progress: false
    steps:
      # actions/deploy-pages <tag-deploy>
      - id: deployment
        uses: actions/deploy-pages@<sha-deploy>
```

- [ ] **Step 2: Substitute the four SHA placeholders**

For each of the four `<sha-...>` and corresponding `<tag-...>` placeholders, paste the values recorded in Task 10. After substitution, run:

```bash
grep -n '<sha-\|<tag-' .github/workflows/docs.yml
```

Expected: no matches.

- [ ] **Step 3: Sanity-check the YAML**

Run:

```bash
python3 -c "import yaml, sys; yaml.safe_load(open('.github/workflows/docs.yml')); print('YAML OK')"
```

Expected: `YAML OK`. (Any Python 3 with PyYAML installed works; if PyYAML is unavailable, use any equivalent YAML linter or skip — GitHub will reject malformed YAML at workflow-load time.)

- [ ] **Step 4: Optionally lint with actionlint**

If `actionlint` is installed (`brew install actionlint` on macOS), run:

```bash
actionlint .github/workflows/docs.yml
```

Expected: no errors. (This step is optional; the GitHub-side validation in Task 17 will catch any issues that actionlint misses.)

---

## Task 12: Update `.github/rulesets/main-protection-checks.json`

**Files:**
- Modify: `.github/rulesets/main-protection-checks.json`

- [ ] **Step 1: Read the current file**

Run:

```bash
cat .github/rulesets/main-protection-checks.json
```

Expected: the file as shown in the spec §9.1, with required_status_checks containing `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`.

- [ ] **Step 2: Add the `book-build` entry**

Edit `.github/rulesets/main-protection-checks.json`, adding `{ "context": "book-build" },` to the `required_status_checks` array. Place it immediately after the `doc-coverage` entry (keeping the file in the order of the spec's §9.1 diff):

```diff
           { "context": "fmt" },
           { "context": "clippy" },
           { "context": "test (ubuntu-latest, stable)" },
           { "context": "docs" },
           { "context": "doc-coverage" },
+          { "context": "book-build" },
           { "context": "commits" },
           { "context": "pr-title" },
           { "context": "audit" },
           { "context": "deny" }
```

- [ ] **Step 3: Validate the JSON**

Run:

```bash
python3 -m json.tool .github/rulesets/main-protection-checks.json > /dev/null && echo "JSON OK"
```

Expected: `JSON OK`.

---

## Task 13: Commit the workflow and ruleset change

**Files:** none

- [ ] **Step 1: Stage and review**

Run:

```bash
git add .github/workflows/docs.yml .github/rulesets/main-protection-checks.json
git status --short
git diff --cached
```

Expected: one new file (`docs.yml`), one modified file (`main-protection-checks.json` with a single `+` line for `book-build`). No `<sha-...>` placeholders anywhere in the diff.

- [ ] **Step 2: Commit**

Run:

```bash
git commit -m "$(cat <<'EOF'
docs(repo): SMA-311 add Pages deploy workflow and required-check entry

Adds .github/workflows/docs.yml with two jobs:
- book-build: runs on PR and main, installs mdbook + mdbook-linkcheck
  via taiki-e/install-action, builds docs/book/ (linkcheck enforced
  via the [output.linkcheck] backend), uploads the Pages artifact on
  main pushes only.
- book-deploy: deploys via actions/deploy-pages on main pushes,
  permission-scoped to pages:write + id-token:write.

Adds book-build to .github/rulesets/main-protection-checks.json so
the build gate becomes enforceable on main once the ruleset is
re-applied post-merge.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds. The Conventional Commits check via `convco` (local commit-msg hook and the `commits` CI check) should pass — `docs(repo)` is in the `.versionrc` scopeRegex allowlist.

---

## Task 14: Update `README.md`

**Files:**
- Modify: `README.md` (the "Documentation" section, lines 46–48 at time of writing)

- [ ] **Step 1: Read the current Documentation section**

Run:

```bash
sed -n '46,48p' README.md
```

Expected:

```
## Documentation

The architectural reference lives in Notion: ["Crate Reference"](https://www.notion.so/355830e8fbaa813c92e8c1aa9985fd3f). An mdBook-hosted equivalent will replace the Notion page once published.
```

(If the line numbers have drifted, locate the `## Documentation` heading and the single paragraph beneath it.)

- [ ] **Step 2: Replace the paragraph**

Replace the single-paragraph "Documentation" body with:

```markdown
The public documentation site is published at <https://smk1085.github.io/paigasus-helikon/>. It is currently a scaffold — full chapters land alongside their feature tickets.

To build it locally: `cd docs/book && mdbook serve` (requires `mdbook` and `mdbook-linkcheck` installed via `cargo install`; see [CONTRIBUTING.md](./CONTRIBUTING.md#documentation-site-mdbook) for exact versions).

The architectural source-of-truth currently lives in Notion: ["Crate Reference"](https://www.notion.so/355830e8fbaa813c92e8c1aa9985fd3f). Content migrates into the published book as the SDK lands.
```

- [ ] **Step 3: Verify the edit**

Run:

```bash
grep -A 6 '^## Documentation$' README.md
```

Expected: the heading followed by the three paragraphs above.

---

## Task 15: Update `CONTRIBUTING.md`

**Files:**
- Modify: `CONTRIBUTING.md`

- [ ] **Step 1: Locate the "Common commands" section**

Run:

```bash
grep -n '^## Common commands\|^### ' CONTRIBUTING.md | head -20
```

This shows the structure of the file. Find the `## Common commands` section heading and the next `## ` heading after it; the new "Documentation site (mdBook)" subsection goes at the end of "Common commands", before the next top-level section.

- [ ] **Step 2: Insert the new subsection**

Add this content at the end of the "Common commands" section (and before the next `## ` heading):

````markdown
### Documentation site (mdBook)

The public docs site is built from `docs/book/`.

```bash
cargo install mdbook --version 0.4.43 --locked
cargo install mdbook-linkcheck --version 0.7.7 --locked
cd docs/book && mdbook serve
```

`mdbook serve` opens `http://localhost:3000` with live-reload. The CI `book-build` job runs `mdbook build`, which includes linkcheck because the `[output.linkcheck]` backend is declared in `book.toml`; broken internal links fail the build.

Deployment to GitHub Pages happens automatically on push to `main` via `.github/workflows/docs.yml`. The Pages source must be set to **GitHub Actions** in the repo's Settings → Pages — this is a one-time manual step performed during the SMA-311 PR merge.

Note: with the `[output.linkcheck]` backend active, the rendered HTML lands under `docs/book/book/html/` (not `docs/book/book/`).
````

- [ ] **Step 3: Verify the edit**

Run:

```bash
grep -A 2 '^### Documentation site (mdBook)$' CONTRIBUTING.md
```

Expected: the new subsection heading followed by the intro line.

---

## Task 16: Final local verification

**Files:** none

- [ ] **Step 1: Re-build the book**

Run:

```bash
cd docs/book && mdbook build && cd -
```

Expected: exit 0, linkcheck clean. This re-runs the gate that CI will run.

- [ ] **Step 2: Sanity-check the full file inventory**

Run:

```bash
git status --short
git diff --stat HEAD~3
```

Expected:
- `git status` shows only `README.md` and `CONTRIBUTING.md` as modified (the previous two commits already landed `docs/book/` and the workflow).
- `git diff --stat HEAD~3` shows additions for `docs/book/**`, `.github/workflows/docs.yml`, `.github/rulesets/main-protection-checks.json`, and the README + CONTRIBUTING modifications.

- [ ] **Step 3: Confirm no stray `<sha-` or `<tag-` placeholders anywhere**

Run:

```bash
grep -rn '<sha-\|<tag-' .github/ docs/book/ README.md CONTRIBUTING.md || echo "OK: no placeholders"
```

Expected: `OK: no placeholders`.

---

## Task 17: Commit the README + CONTRIBUTING edits

**Files:** none

- [ ] **Step 1: Stage and commit**

Run:

```bash
git add README.md CONTRIBUTING.md
git commit -m "$(cat <<'EOF'
docs(repo): SMA-311 cross-link docs site from README and CONTRIBUTING

- README.md: replace the single-paragraph Documentation section with
  the published-site URL, local-build instructions, and a note that
  Notion remains the architectural source-of-truth until content
  migrates.
- CONTRIBUTING.md: new "Documentation site (mdBook)" subsection under
  Common commands, covering local install, mdbook serve, the
  linkcheck-induced book/html/ output path, and the one-time Pages
  settings toggle that lands during PR merge.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: commit succeeds.

- [ ] **Step 2: Verify the three-commit branch**

Run:

```bash
git log --oneline main..HEAD
```

Expected (most-recent first):
1. `docs(repo): SMA-311 cross-link docs site from README and CONTRIBUTING`
2. `docs(repo): SMA-311 add Pages deploy workflow and required-check entry`
3. `docs(repo): SMA-311 scaffold mdBook book (book.toml, SUMMARY, stubs)`
4. (`docs(spec): SMA-311 add design for mdBook docs scaffold` — the design doc commit, from the brainstorming phase)
5. (`docs(plan): SMA-311 add implementation plan for mdBook docs scaffold` — added at plan-writing time, on the same feature branch)

---

## Task 18: Push the branch and prepare PR

**Files:** none

- [ ] **Step 1: Push the branch**

Run:

```bash
git push -u origin feature/sma-311-mdbook-docs-scaffold
```

Expected: branch published. `pre-push` hooks (if any) pass.

Note: the new `docs.yml` workflow uses `on: pull_request` (not `on: push`), so the `book-build` job will not fire on the branch push — it will fire when the PR is opened in Step 2.

- [ ] **Step 2: Open the PR**

Run:

```bash
gh pr create --title "docs(repo): SMA-311 mdBook docs scaffold" --body "$(cat <<'EOF'
## Summary

Scaffolds the public documentation site for paigasus-helikon and the CI/CD path that publishes it:

- New `docs/book/` mdBook (sibling to `docs/superpowers/`), with stub pages for Introduction, Getting Started, ten Concepts pages mirroring the Notion Architecture hub, Reference, and a Decisions index.
- New `.github/workflows/docs.yml` (`book-build` + `book-deploy` jobs) using the native `actions/deploy-pages` flow.
- `book-build` added to `.github/rulesets/main-protection-checks.json`.
- README + CONTRIBUTING cross-link the site and document the local-build flow.

Design: [`docs/superpowers/specs/2026-05-20-sma-311-mdbook-docs-scaffold-design.md`](./docs/superpowers/specs/2026-05-20-sma-311-mdbook-docs-scaffold-design.md).

## Manual steps required at merge time

1. **Before merge:** Set repo Settings → Pages → Source to **GitHub Actions**. Without this, `actions/deploy-pages` fails with `Pages site not found`.
2. **After merge** (once `book-build` has reported on `main`): Re-apply `.github/rulesets/main-protection-checks.json` via Settings → Rules → Rulesets or `gh api -X PUT /repos/SMK1085/paigasus-helikon/rulesets/<id> --input .github/rulesets/main-protection-checks.json` so that `book-build` becomes enforced.

## Test plan

- [ ] `book-build` job is green on this PR.
- [ ] After merge to `main`: visit <https://smk1085.github.io/paigasus-helikon/> and confirm the book renders end-to-end (all SUMMARY.md links resolve).
- [ ] After merge: ruleset re-applied with `book-build` in the required set.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: PR URL printed. Note it for follow-up.

- [ ] **Step 3: Watch CI on the PR**

Run:

```bash
gh pr checks --watch
```

or watch the Actions tab. The new `book-build` job is the one most likely to fail on first push. If it does:
- Linkcheck failure → fix the offending markdown link or path locally, commit, push.
- `taiki-e/install-action` failure → re-check the tool format (`mdbook@VERSION,mdbook-linkcheck@VERSION` comma-separated, no spaces).
- SHA-not-found → re-verify the SHAs from Task 10 and push a correction.

The existing required checks (`fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`) should pass — this PR only touches documentation and CI config, no Rust code. The `pr-title` check validates the title `docs(repo): SMA-311 mdBook docs scaffold` is a valid Conventional Commits title.

Do not merge until every required check is green.

---

## Task 19: Linear hand-off

**Files:** none

- [ ] **Step 1: Confirm SMA-311 is "In Progress"**

The brainstorming session transitioned SMA-311 to In Progress at design time. Verify by visiting <https://linear.app/smaschek/issue/SMA-311/mdbook-docs-scaffold>. No action needed unless the status drifted.

- [ ] **Step 2: After PR merge, do not move the Linear status manually**

Linear auto-closes the linked SMA-311 issue when its PR merges (per the project memory rule). No manual transition is required.

- [ ] **Step 3: Perform the manual Pages-source toggle and ruleset re-apply**

Per the PR description's "Manual steps required at merge time" section. Both are one-time, must happen at merge boundary, and cannot be automated from the PR itself.

---

## Spec coverage check

Spec section → plan task(s):

- §1 Goal → Tasks 2-17 collectively.
- §2 Decisions table → no separate task; decisions are baked into Tasks 2, 5, 7, 11, 12.
- §3 Files added/modified → Tasks 2-7 (added), Tasks 12, 14, 15 (modified).
- §4 Directory layout → Task 2 step 1.
- §5 `book.toml` → Task 2.
- §6 `SUMMARY.md` → Task 3.
- §7 Skeleton format → Tasks 3-7.
- §8 Workflow → Tasks 10-11.
- §8.1 SHA resolution → Task 10.
- §8.2 Tool version pins → Task 1 (install), Task 2 (book.toml), Task 11 (workflow env).
- §9.1 Ruleset → Task 12.
- §9.2 README → Task 14.
- §9.3 CONTRIBUTING → Task 15.
- §10 Manual steps → Task 18 step 3, Task 19 step 3.
- §11 Commit shape → Tasks 9, 13, 17 (three commits).
- §12 Acceptance criteria → Tasks 8, 16, 18 (test plan in PR body).
- §13 Out of scope → enforced by omission.

All spec sections covered.
