# SMA-311 — mdBook docs scaffold — design

- **Linear:** [SMA-311](https://linear.app/smaschek/issue/SMA-311/mdbook-docs-scaffold)
- **Branch:** `feature/sma-311-mdbook-docs-scaffold`
- **Status:** design (awaiting implementation plan)
- **Author:** Sven Maschek
- **Date:** 2026-05-20

## 1. Goal

Stand up the public documentation site for `paigasus-helikon` as an mdBook scaffold, deployed to GitHub Pages at <https://smk1085.github.io/paigasus-helikon/>. The scaffold is intentionally a skeleton: it establishes the chapter structure, build pipeline, deploy pipeline, and link-checking gates so that subsequent feature tickets can land real content into pre-existing pages without re-litigating layout, tooling, or CI plumbing.

This ticket explicitly does **not** migrate content from Notion. Each chapter page is a stub with a one-sentence intent statement and a "Stub" callout. Content migration is tracked separately, per-page, alongside the feature ticket that lands the underlying SDK code.

The acceptance criteria from the Linear ticket are:

- `mdbook serve` renders the book locally.
- GitHub Pages serves the book at `https://smk1085.github.io/paigasus-helikon/`.

Both are met by this design.

## 2. Decisions and rationale

Six decisions made during brainstorming. They shape the rest of the spec.

| Decision | Choice | Rationale |
|---|---|---|
| Book location | **`docs/book/`** (sibling to `docs/superpowers/`) | Cleanly separates the public book from the internal design artifacts under `docs/superpowers/{specs,plans}/`. The ticket-as-written says "`docs/` directory with `book.toml`", which the in-place layout `docs/book.toml` also satisfies, but the sibling subdirectory makes the two concerns unambiguous and gives the workflow a clean `working-directory: docs/book` anchor. |
| Skeleton depth | **Stub pages** (H1 + 1–2 sentence intent + `> **Stub.**` callout) | Cheapest to land and lowest staleness risk. Pulling Notion bodies in now would invite rewrites the moment SDK APIs materialize. Pure-empty pages (just an H1) were rejected because they leave search engines and human readers with nothing. |
| Decisions chapter | **Single index page pointing to internal specs** | Honest about current state: architectural decisions live as design docs under `docs/superpowers/specs/` until the SDK ships a user-facing release. Adopting MADR/ADR now would be format-cargo-cult before there's a single public-API decision to record. |
| Toolchain | **`taiki-e/install-action` for `mdbook` + `mdbook-linkcheck`** | Matches the repo's existing pattern (the SMA-335 `commits` job already uses `taiki-e/install-action` for `convco`). Pre-built binaries are faster than `cargo install`. Including `mdbook-linkcheck` from day one catches SUMMARY.md / internal-link breakage immediately — and the scaffold has the most internal links it ever will relative to body content. |
| Deploy method | **Native `actions/deploy-pages`** (Pages source = "GitHub Actions") | Modern supported pattern. No `gh-pages` branch pollution, no per-deploy commit. The trade-off is a one-time manual step to set Pages source in repo settings — documented in CONTRIBUTING.md and the PR description. |
| Required status check | **Add `book-build` to `.github/rulesets/main-protection-checks.json`** | The build job is fast, deterministic, and runs on every PR — exactly the shape of an enforceable gate. The deploy job cannot be a required check (only runs on push to `main`). Matches the SMA-309 / SMA-306 ruleset pattern. |
| PR previews | **Out of scope** | Per-PR preview deploys add real complexity (preview cleanup, branch-name collisions, bot comments) for marginal benefit at the scaffold stage. Reviewers can `mdbook serve` locally or download the `github-pages` artifact from a `main` run. Punt to a follow-up ticket if the need materializes. |

A naming consequence of the existing CI gates: `.github/workflows/ci.yml` already declares a job called `docs` (rustdoc HTML emission with `-D warnings`), which is in the required-status-check list. The new mdBook job is named **`book-build`** to avoid collision. This is the only place the design departs from a "name the obvious thing the obvious name" instinct.

## 3. Files added / modified

### Added

| Path | Purpose |
|---|---|
| `docs/book/book.toml` | mdBook config: title, `site-url = "/paigasus-helikon/"`, html + linkcheck backends, `git-repository-url`, `edit-url-template`. See §5. |
| `docs/book/.gitignore` | Single line: `book/` — ignores mdBook's build output directory. Conventional for mdBook books; kept local rather than polluting the repo-root `.gitignore`. |
| `docs/book/src/SUMMARY.md` | Chapter tree. See §6. |
| `docs/book/src/introduction.md` | Landing page. ~5 sentences: pitch + "what's here / what's not yet here" callout. See §7. |
| `docs/book/src/getting-started/quickstart.md` | Stub. |
| `docs/book/src/getting-started/workspace-layout.md` | Stub. |
| `docs/book/src/concepts/core-primitives.md` | Stub. One-sentence intent: "The seven traits (`Model`, `Tool`, `Agent`, `Session`, `Guardrail`, `Hook`, `Runner`) and the concrete types they share." |
| `docs/book/src/concepts/agent-loop.md` | Stub. "Why the loop is an explicit `enum LoopState`, and how it drives the typed event stream." |
| `docs/book/src/concepts/tools.md` | Stub. "The `Tool` trait, the `#[tool]` macro, schemars-derived JSON Schema, and heterogeneous tool registries." |
| `docs/book/src/concepts/model-providers.md` | Stub. "The single `Model` trait, `ModelCapabilities`, provider crates, and the async-trait vs AFIT trade-off." |
| `docs/book/src/concepts/sessions.md` | Stub. "Append-only event log, projections, default backends, and the compaction wrapper." |
| `docs/book/src/concepts/multi-agent-patterns.md` | Stub. "`LlmAgent`, Sequential / Parallel / Loop, Swarm, Graph, Agent-as-Tool, and Handoffs." |
| `docs/book/src/concepts/permissions-guardrails-hooks.md` | Stub. "Permission modes, decisions, guardrail tripwires, and lifecycle hooks." |
| `docs/book/src/concepts/mcp-integration.md` | Stub. "`rmcp` client wrapper, lazy tool loading, and exposing agents as MCP servers." |
| `docs/book/src/concepts/observability-evaluation.md` | Stub. "OTel + GenAI conventions; eval hooks (replay, recorded traces, trajectory)." |
| `docs/book/src/concepts/structured-output-builder.md` | Stub. "`output_type::<T>()`, schemars + retry/repair, and the typestate builder." |
| `docs/book/src/reference/crates.md` | Stub. Will eventually summarize the 13-crate workspace; for now a one-sentence intent + Stub callout. |
| `docs/book/src/reference/api-docs.md` | Stub. Will point to docs.rs once the workspace is published; for now a one-sentence "API docs land on docs.rs after the first published release" note. |
| `docs/book/src/decisions/index.md` | Index page pointing at `docs/superpowers/specs/` on GitHub. See §7. |
| `.github/workflows/docs.yml` | Two-job workflow: `book-build` (PR + main) and `book-deploy` (main only). See §8. |

### Modified

| Path | Change |
|---|---|
| `README.md` | Replace the existing "Documentation" paragraph with three short paragraphs: live URL, local-build instructions, and a note that Notion remains the architectural source-of-truth until content migrates. See §9.2. |
| `CONTRIBUTING.md` | Add a "Documentation site (mdBook)" subsection under "Common commands". See §9.3. |
| `.github/rulesets/main-protection-checks.json` | Add `{ "context": "book-build" }` to the `required_status_checks` array. See §9.1. |

### Not modified

- **Root `.gitignore`** — the per-directory `docs/book/.gitignore` keeps the rule local; this is conventional for mdBook books vendored alongside other content.
- **`.github/CODEOWNERS`** — the existing `* @SMK1085` line already owns the new files.
- **Workspace `Cargo.toml`** — mdBook is not a Cargo dependency of the workspace; it's a developer tool installed separately.
- **`CLAUDE.md`** — no new non-obvious convention warrants a callout. The mdBook layout, the workflow naming choice (`book-build` to avoid colliding with the existing `docs` rustdoc job), and the Pages-settings manual step are all sufficiently documented in `CONTRIBUTING.md` and the PR description.

## 4. Directory layout

```
docs/
├── superpowers/           # unchanged (internal design artifacts)
│   ├── plans/
│   └── specs/
└── book/                  # NEW — the public docs site
    ├── book.toml
    ├── .gitignore         # ignores ./book/ (mdBook output dir)
    └── src/
        ├── SUMMARY.md
        ├── introduction.md
        ├── getting-started/
        │   ├── quickstart.md
        │   └── workspace-layout.md
        ├── concepts/
        │   ├── core-primitives.md
        │   ├── agent-loop.md
        │   ├── tools.md
        │   ├── model-providers.md
        │   ├── sessions.md
        │   ├── multi-agent-patterns.md
        │   ├── permissions-guardrails-hooks.md
        │   ├── mcp-integration.md
        │   ├── observability-evaluation.md
        │   └── structured-output-builder.md
        ├── reference/
        │   ├── crates.md
        │   └── api-docs.md
        └── decisions/
            └── index.md
```

## 5. `book.toml`

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

Three things worth knowing about this config:

1. **`site-url = "/paigasus-helikon/"`** is required because GitHub Pages serves the site under a project-name subpath, not at the domain root. Without this, every internal asset 404s.
2. **Both `[output.html]` and `[output.linkcheck]` are declared.** Declaring linkcheck as an output backend (rather than running it as a separate command) makes mdBook execute both backends on every `mdbook build`. Broken internal links fail the build. The side-effect is that the HTML output moves from `docs/book/book/` to `docs/book/book/html/` — the workflow's artifact path accounts for that (§8).
3. **`follow-web-links = false`** keeps the linkcheck pass offline and deterministic. Catching dead external URLs is valuable but belongs in a periodic linkcheck workflow, not on every PR. Out of scope for this ticket.

## 6. `SUMMARY.md`

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

The Concepts ordering mirrors the Notion "Architecture" hub's table verbatim. The hub also suggests a first-pass reading order (Core Primitives → Agent Loop → Tools → Sessions) that crosses table positions; the published book preserves the table order rather than the reading order, on the assumption that readers will scan top-to-bottom and follow links.

## 7. Skeleton page format

### 7.1 Uniform stub template (applies to every Concepts / Reference / Getting Started page)

```markdown
# <Page Title>

<one-sentence intent statement, taken verbatim from the Notion Architecture hub for Concepts pages, or freshly written for the others>

> **Stub.** Full content lands with the corresponding implementation ticket. The architectural source-of-truth for this section currently lives in internal design notes; it will move here as the SDK matures.
```

The stub callout is identical on every page so it's visually obvious which pages are placeholders and which are content. Future feature tickets remove the callout when they land real content.

**No outbound links to Notion** from any public page. Notion pages are likely private; linking would 404 for public readers. Notion is referenced only in `CLAUDE.md`, Linear, and now this spec — all of which are internal.

### 7.2 Introduction page (longer than a stub, but still skeleton)

```markdown
# Introduction

`paigasus-helikon` is a Rust SDK for building agentic AI systems. It separates slow-moving primitives (types, traits, message protocols) from fast-moving parts (provider SDKs, execution runtimes, tool catalogs), so downstream projects can pick the surface they need without dragging in the rest.

The SDK does not pick a deployment story, a hosting story, or an observability stack for you. Bring your own.

## What's here

This documentation site is published from the [`paigasus-helikon`](https://github.com/SMK1085/paigasus-helikon) repository. It is currently a **scaffold** — the chapter structure is in place, but most pages are stubs. Real content lands page-by-page alongside the corresponding feature tickets.

## What's not yet here

API documentation lives on [docs.rs](https://docs.rs) once the workspace is published. Internal architectural design notes live in Notion until they migrate here. Tracked work lives in [Linear](https://linear.app/smaschek) under the project **Paigasus Helikon**.
```

### 7.3 Decisions index page

```markdown
# Decisions

Architectural decisions are currently captured as **design docs alongside their Linear tickets**, stored under [`docs/superpowers/specs/`](https://github.com/SMK1085/paigasus-helikon/tree/main/docs/superpowers/specs) in the repository.

Once the SDK ships its first user-facing release, decisions affecting the public API will graduate to a formal ADR/MADR section here.
```

## 8. Workflow: `.github/workflows/docs.yml`

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
      # actions/checkout v6.x — SHA resolved at implementation time
      - uses: actions/checkout@<sha>  # v6.x
      # taiki-e/install-action v2.x — SHA resolved at implementation time
      - uses: taiki-e/install-action@<sha>  # v2.x
        with:
          tool: mdbook@${{ env.MDBOOK_VERSION }},mdbook-linkcheck@${{ env.MDBOOK_LINKCHECK_VERSION }}
      - name: Build mdBook (HTML + linkcheck)
        working-directory: docs/book
        run: mdbook build
      # actions/upload-pages-artifact v3.x — SHA resolved at implementation time
      - name: Upload Pages artifact
        if: github.ref == 'refs/heads/main' && github.event_name == 'push'
        uses: actions/upload-pages-artifact@<sha>  # v3.x
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
      # actions/deploy-pages v4.x — SHA resolved at implementation time
      - id: deployment
        uses: actions/deploy-pages@<sha>  # v4.x
```

Design notes:

- **`book-build` runs on every PR** so reviewers see the same gate maintainers do. The linkcheck pass runs inside `mdbook build` because of the `[output.linkcheck]` backend declaration in `book.toml`. Broken internal links fail the job.
- **Upload-artifact is gated to `main`-only push events.** GitHub Pages enforces one artifact per workflow run; uploading on PRs would be wasted work and could interfere with the deploy job's expectations.
- **`book-deploy` is a separate job** so the `book-build` PR status check stays clean and doesn't carry pages-write permission on PR events.
- **`concurrency: { group: pages, cancel-in-progress: false }`** on the deploy job is the pattern required by `actions/deploy-pages` to prevent two rapid `main` pushes from racing the Pages backend.
- **Top-level permissions are `contents: read`.** The deploy job locally elevates to `pages: write` + `id-token: write`. Principle of least privilege, matching SMA-306's `audit.yml` and the `pr-title.yml` shape.

### 8.1 Action SHA resolution (deferred to implementation time)

Four third-party actions need SHA pins via `gh api repos/<owner>/<repo>/releases/latest` (per CLAUDE.md "implement GitHub Actions against the latest stable major"):

- `actions/checkout`
- `taiki-e/install-action`
- `actions/upload-pages-artifact`
- `actions/deploy-pages`

The implementation plan pins each to the latest stable major at execution time, with the human-readable version in a trailing `# vX.Y.Z` comment.

### 8.2 Tool version pins

`MDBOOK_VERSION` and `MDBOOK_LINKCHECK_VERSION` are manually maintained. Dependabot's `cargo` and `github-actions` ecosystems don't track these. A periodic drive-by bump is acceptable; mdBook changes rarely and the linkcheck preprocessor changes even more rarely. The current pinned versions in this spec (`0.4.43`, `0.7.7`) should be re-verified at implementation time against `cargo search mdbook` and `cargo search mdbook-linkcheck` and bumped to the latest stable if newer.

## 9. Repo configuration updates

### 9.1 `.github/rulesets/main-protection-checks.json`

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

The `docs` context refers to the rustdoc job in `ci.yml`, not the new mdBook job. The new context is `book-build`. The deploy job is not added — it only runs on push to `main`, and required checks must be reportable on PRs.

Per CONTRIBUTING.md's "Repo configuration" section, this JSON file is the canonical declaration but does not auto-sync. After the PR merges and `book-build` has reported at least once on `main` (so GitHub recognizes the context), the ruleset must be re-applied via `gh api -X PUT /repos/SMK1085/paigasus-helikon/rulesets/<id> --input .github/rulesets/main-protection-checks.json` (or the Settings UI). The PR description will call this out.

### 9.2 `README.md` — replace "Documentation" section

```diff
 ## Documentation

-The architectural reference lives in Notion: ["Crate Reference"](https://www.notion.so/355830e8fbaa813c92e8c1aa9985fd3f). An mdBook-hosted equivalent will replace the Notion page once published.
+The public documentation site is published at <https://smk1085.github.io/paigasus-helikon/>. It is currently a scaffold — full chapters land alongside their feature tickets.
+
+To build it locally: `cd docs/book && mdbook serve` (requires `mdbook` and `mdbook-linkcheck` installed via `cargo install`; see [CONTRIBUTING.md](./CONTRIBUTING.md#documentation-site-mdbook) for exact versions).
+
+The architectural source-of-truth currently lives in Notion: ["Crate Reference"](https://www.notion.so/355830e8fbaa813c92e8c1aa9985fd3f). Content migrates into the published book as the SDK lands.
```

### 9.3 `CONTRIBUTING.md` — new subsection under "Common commands"

```markdown
### Documentation site (mdBook)

The public docs site is built from `docs/book/`.

\`\`\`bash
cargo install mdbook --version 0.4.43 --locked
cargo install mdbook-linkcheck --version 0.7.7 --locked
cd docs/book && mdbook serve
\`\`\`

`mdbook serve` opens `http://localhost:3000` with live-reload. The CI `book-build` job runs `mdbook build`, which includes linkcheck because the `[output.linkcheck]` backend is declared in `book.toml`; broken internal links fail the build.

Deployment to GitHub Pages happens automatically on push to `main` via `.github/workflows/docs.yml`. The Pages source must be set to **GitHub Actions** in the repo's Settings → Pages — this is a one-time manual step performed during the SMA-311 PR merge.
```

(The fenced code block above is rendered with backslash-escaped backticks for the spec; the implementation will use plain backticks.)

## 10. Manual steps and risks

### 10.1 One-time manual setup (cannot be automated from the PR)

| Step | Where | When |
|---|---|---|
| Set Pages source to **GitHub Actions** | Repo Settings → Pages | Once, before merge. Without this, `actions/deploy-pages` fails with `Pages site not found`. |
| Re-apply `.github/rulesets/main-protection-checks.json` so `book-build` becomes enforced | Repo Settings → Rules → Rulesets → main-protection-checks (or `gh api -X PUT`) | After merge, once `book-build` has reported on `main` so GitHub recognizes the context. |

Both are documented in the PR description and CONTRIBUTING.md (§9.3).

### 10.2 Risks accepted

- **Output path is `docs/book/book/html/`, not `docs/book/book/`.** Side-effect of the linkcheck backend. The workflow accounts for it; CONTRIBUTING.md documents it for anyone running `mdbook build` locally and hunting for the rendered site.
- **mdBook and mdbook-linkcheck versions are manually pinned.** Six-monthly drive-by bumps are the maintenance cost.
- **Skeleton pages are visible from day one.** Intentional — the acceptance criteria require a live published site. The per-page "Stub" callout is honest about state.
- **No PR previews.** Reviewers `mdbook serve` locally, or download the `github-pages` artifact from a `main` workflow run.
- **Deploy job runs on every push to `main`**, even documentation-irrelevant ones. `mdbook build` of a stub site takes seconds. If it ever becomes a problem, a `paths:` filter on the workflow trigger is the surgical fix.

## 11. Commit shape

Single PR on `feature/sma-311-mdbook-docs-scaffold`. Commit type for the implementation commit(s): `docs(book): SMA-311 ...`.

Per CLAUDE.md's "bootstrap commits on release infrastructure must use `chore(...)` or `docs(...)`" rule, **no `feat(...)` or `fix(...)` commits in this PR.** This is documentation infrastructure; a `feat` commit would mis-attribute a version bump across the workspace via release-plz.

This spec document itself lands on the same feature branch (not pre-merged to `main`) as `docs(spec): SMA-311 add design for mdBook docs scaffold`.

## 12. Acceptance criteria (verification plan)

Before requesting review on the implementation PR:

1. `cd docs/book && mdbook build` — exits 0 with linkcheck clean.
2. `cd docs/book && mdbook serve` — opens `http://localhost:3000` and renders the SUMMARY.md tree end-to-end (every link in SUMMARY.md resolves to its stub page).
3. The `book-build` job on the PR passes.
4. After merge: <https://smk1085.github.io/paigasus-helikon/> renders the book.
5. After merge and the first successful `book-build` on `main`: the ruleset re-apply step adds `book-build` to the enforced required-status-check set.

## 13. Out of scope

- Content migration from Notion (per-page, per-feature-ticket).
- Per-PR preview deploys.
- External (web) link checking on every PR. (Could be a separate scheduled workflow later.)
- Custom theme, logo, or visual branding beyond `default-theme = "rust"` + `preferred-dark-theme = "ayu"`.
- docs.rs integration (waits on the workspace being published to crates.io).
- A formal ADR/MADR section. (Decisions chapter is an index pointer until the SDK ships its first public release.)
- Search backend configuration beyond mdBook's built-in client-side search (which is on by default).
