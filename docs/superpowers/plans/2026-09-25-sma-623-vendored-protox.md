# SMA-623 Phase 1 — protox for the Temporal protos — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the workspace with no system `protoc`: enable `temporalio-client`'s `vendored-protox` feature, remove `setup-protoc` from the 8 non-publishing CI sites, add two regression guards, and bring the documents into line.

**Architecture:** One manifest feature switches the two protoc-calling build scripts (`temporalio-protos`, `prost-wkt-types`) to the pure-Rust `protox` compiler. CI stops installing `protoc` everywhere except `release-plz.yml`, which keeps it until SMA-687 (phase 2). A `cargo tree` assertion and a `PROTOC` path that does not exist make a regression fail the required gates.

**Tech Stack:** Cargo workspace (Rust, MSRV 1.94), GitHub Actions YAML, Markdown.

**Spec:** `docs/superpowers/specs/2026-09-25-sma-623-vendored-protox-design.md` (approved at Gate 1). Read it before you start. Section numbers below refer to it.

## Global Constraints

- Work only in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.worktrees/sma-623`, branch `feature/sma-623-evaluate-temporalio-protos-vendored-protox-to-retire-the`. Use absolute paths under that root for every file edit.
- Never run `git checkout`, `git switch`, `git reset`, `git stash`, `git rebase`, or `git push`. Never run `git add -A` or `git add .` (`.env` is not ignored). Add files by explicit path.
- Do **not** edit `.github/workflows/release-plz.yml`, `release-plz.toml`, or anything under `.github/actions/setup-protoc/`. They are phase 2 (SMA-687).
- Do **not** change any crate `version` field. release-plz does the bumps.
- Do **not** run a bare `cargo update`. The lockfile may only gain packages reachable from `protox`.
- Commit messages: `<type>(<scope>): SMA-623 <lowercase subject>`, scopes from `.versionrc`. End every commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Commits are signed through 1Password; if a commit fails with "failed to fill whole buffer", stop and report — do not bypass signing.
- Run every cargo command in the **foreground** and wait for it. Do not background builds, do not use Monitor, and do not end your turn before every step has a terminal result.
- New prose (Markdown, comments) uses ASD-STE100 Simplified Technical English: short sentences, active voice, one idea per sentence, no idiom.
- The local machine has a Homebrew `protoc` on `PATH`. To prove that a build needs no `protoc`, set `PROTOC=/nonexistent/protoc`. prost-build reads `PROTOC` before `PATH`.
- The scripts `scripts/check-*.sh` need bash >= 4 (`mapfile`). On macOS, run them with Homebrew bash (`/opt/homebrew/bin/bash`), not `/bin/bash` 3.2.

## Review Focus

1. **Guard 1 pattern:** if the exact text that `cargo tree -e features -i prost-wkt-types` prints differs from `feature "vendored-protox"`, the guard is always red or always green. Task 2 Step 2 and Step 7 prove it both ways.
2. **Windows and the `PROTOC` guard:** `/nonexistent/protoc-see-SMA-623` is not a Windows path. The build must still pass there because no build script calls `protoc`. Proof comes from `test (windows-latest, stable)` on the PR; Task 2 must not add any Windows-specific path handling.
3. **Broken workflow YAML:** an indentation error in `ci.yml` makes every required job disappear, and GitHub reports a workflow-file error, not a failed check. Task 2 Step 8 parses every edited workflow.
4. **Lockfile drift:** a normal `cargo build` can also move unrelated packages if the lock is stale. Task 1 Step 6 rejects any changed existing version.
5. **Stale `protoc` statements in documents** that tell a contributor to install `protoc`. Task 4 Step 9 greps every document location.

---

### Task 1: Enable `vendored-protox` (manifest, lockfile, crate README)

**Files:**
- Modify: `Cargo.toml` (root, `[workspace.dependencies]` Temporal block, lines 115-142)
- Modify: `Cargo.lock` (cargo writes it)
- Modify: `crates/paigasus-helikon-runtime-temporal/README.md` (after the "Install" section's second code block)

**Interfaces:**
- Consumes: nothing.
- Produces: the feature `vendored-protox` is active on `temporalio-protos` and `prost-wkt-types` in the resolved graph. Task 2's Guard 1 asserts exactly this.

- [ ] **Step 1: Prove the failure (the "failing test")**

Run:

```bash
cd /Users/smaschek/dev/paigasus/paigasus-helikon/.worktrees/sma-623
cargo tree -p paigasus-helikon-runtime-temporal -e features -i prost-wkt-types | grep 'vendored-protox' ; echo "grep exit: $?"
```

Expected: no matching line, `grep exit: 1`.

Then run:

```bash
PROTOC=/nonexistent/protoc cargo build -p paigasus-helikon-runtime-temporal 2>&1 | tail -20
```

Expected: FAIL with `Could not find \`protoc\`` from `prost-wkt-types` and/or `temporalio-protos`. (If the build units are already cached from an earlier build and the build passes, run `cargo clean -p temporalio-protos -p prost-wkt-types` and repeat.)

- [ ] **Step 2: Edit the manifest**

In the root `Cargo.toml`, find these two lines:

```toml
# through feature unification. Do not "clean it up".
temporalio-sdk      = { version = "1.0", default-features = false }
temporalio-client   = { version = "1.0", default-features = false, features = ["tls-aws-lc"] }
```

Replace them with:

```toml
# through feature unification. Do not "clean it up".
# `vendored-protox` (SMA-623): temporalio-protos and prost-wkt-types compile their
# .proto files with `protox` (pure Rust) instead of a system `protoc`, so neither
# this workspace nor its downstream users need protoc. The feature must stay on
# temporalio-client (or temporalio-common): temporalio-sdk and temporalio-sdk-core
# do not forward it. protox and its dependencies are BUILD dependencies, so the
# `-e normal` ring check above does not see them. Two ci.yml guards fail the
# required gates if the feature disappears: a `cargo tree` assertion in
# `build-no-default-features`, and `PROTOC` set to a path that does not exist.
temporalio-sdk      = { version = "1.0", default-features = false }
temporalio-client   = { version = "1.0", default-features = false, features = ["tls-aws-lc", "vendored-protox"] }
```

- [ ] **Step 3: Let cargo update the lockfile and prove the pass**

Run (NOT `--locked`, NOT `cargo update`):

```bash
PROTOC=/nonexistent/protoc cargo build -p paigasus-helikon-runtime-temporal 2>&1 | tail -5
```

Expected: `Finished` with exit 0.

- [ ] **Step 4: Confirm the feature in the graph**

Run:

```bash
cargo tree -p paigasus-helikon-runtime-temporal -e features -i prost-wkt-types | grep 'vendored-protox'
```

Expected: at least one line that contains `feature "vendored-protox"`. **Copy the exact matching line into your report** — Task 2's Guard 1 greps for the text `feature "vendored-protox"`. If the printed form is different (for example, no quotes), report the exact form; Task 2 must then use it.

- [ ] **Step 5: Run the crate tests without protoc**

```bash
PROTOC=/nonexistent/protoc cargo test -p paigasus-helikon-runtime-temporal 2>&1 | grep -E '^test result|FAILED|panicked' 
```

Expected: every `test result:` line shows `0 failed`. (The spec measured 86 unit, 6 `temporal_live`, 2 doc tests passing.)

- [ ] **Step 6: Check the lockfile diff**

```bash
git diff --stat -- Cargo.lock
git diff -- Cargo.lock | grep -E '^[-+]version = ' 
git diff -- Cargo.lock | grep -E '^\+name = '
```

Expected:
- The `^[-+]version` output has **no `-version` lines** (no existing package changed version). Only `+version` lines for new packages.
- The `+name` lines are only packages reachable from `protox`. The spec (M12) expects: `beef`, `logos` (×2), `logos-codegen` (×2), `logos-derive` (×2), `miette`, `miette-derive`, `prost-reflect`, `protox`, `protox-parse`, `unicode-width`. A newer patch version than M12 is acceptable. Any other name, or any `-version` line: STOP and report.
- Confirm reachability for any unexpected name: `cargo tree -i <name> --all-features -e all --target all` must show a path through `protox`.

Put the list of added packages (name + version) in your report; the PR body needs it.

- [ ] **Step 7: The `ring` acceptance check**

```bash
cargo tree -i ring --all-features -e normal --target all
```

Expected: `warning: nothing to print.`

- [ ] **Step 8: Add the README sentence**

In `crates/paigasus-helikon-runtime-temporal/README.md`, find:

````markdown
```bash
cargo add paigasus-helikon --features runtime-temporal
```

## Quick start
````

Replace it with:

````markdown
```bash
cargo add paigasus-helikon --features runtime-temporal
```

The crate does not need a system `protoc`. It compiles the Temporal protobuf definitions with [`protox`](https://crates.io/crates/protox), a protobuf compiler in pure Rust, through the `vendored-protox` feature of `temporalio-client`.

## Quick start
````

- [ ] **Step 9: Format check and commit**

```bash
cargo fmt --all -- --check
git add Cargo.toml Cargo.lock crates/paigasus-helikon-runtime-temporal/README.md
git commit -m "feat(runtime-temporal): SMA-623 compile temporal protos with protox

Enable temporalio-client's vendored-protox feature. temporalio-protos and
prost-wkt-types now compile their .proto files with protox, so the build
needs no system protoc.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
git show --stat HEAD
```

Expected: the commit contains exactly these three files.

---

### Task 2: CI — remove `setup-protoc` from 8 sites and add the two guards

**Files:**
- Modify: `.github/workflows/ci.yml` (env block lines 23-31; `clippy` 72-75; `test` 119-124; `build-no-default-features` 175-178 and after 200; `docs` 214-217; `doc-coverage` 245-248; `sessions-it` 291-294 and 304-308)
- Modify: `.github/workflows/msrv.yml` (lines 44-47)
- Modify: `.github/workflows/integration.yml` (lines 64-65, 102-104, 127-133)

**Interfaces:**
- Consumes: Task 1's resolved feature; the exact `cargo tree` line from Task 1 Step 4.
- Produces: no `./.github/actions/setup-protoc` use outside `release-plz.yml`; workflow env `PROTOC: /nonexistent/protoc-see-SMA-623` in `ci.yml`; a step named `Assert vendored-protox is active for the Temporal protos` in `build-no-default-features`.

- [ ] **Step 1: Record the sites before the change**

```bash
grep -rn 'setup-protoc' .github/workflows
```

Expected: 9 `uses:` hits (`ci.yml` ×6, `msrv.yml`, `integration.yml`, `release-plz.yml`) plus comment hits.

- [ ] **Step 2: Prove Guard 1 fails without the feature (the "failing test")**

Back up the manifest **and** the lockfile (with the feature off, `cargo tree` rewrites `Cargo.lock` and drops the `protox` entries). Run the guard against a manifest without the feature. Then restore both files by copy:

```bash
B=/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/03aabda2-ef7c-4548-b97c-5899ae865357/scratchpad
cp Cargo.toml "$B/Cargo.toml.sma623.bak"
cp Cargo.lock "$B/Cargo.lock.sma623.bak"
sed -i '' 's/features = \["tls-aws-lc", "vendored-protox"\] }/features = ["tls-aws-lc"] }/' Cargo.toml
grep -n '^temporalio-client' Cargo.toml
if ! cargo tree -p paigasus-helikon-runtime-temporal -e features -i prost-wkt-types | grep -q 'feature "vendored-protox"'; then echo "GUARD FAILS (expected)"; else echo "GUARD PASSES (WRONG)"; fi
cp "$B/Cargo.toml.sma623.bak" Cargo.toml
cp "$B/Cargo.lock.sma623.bak" Cargo.lock
git diff --quiet -- Cargo.toml Cargo.lock && echo "manifest and lock restored"
```

Expected: the `grep` shows `features = ["tls-aws-lc"] }`, then `GUARD FAILS (expected)`, then `manifest and lock restored`. If the last line does not print, STOP and report `git diff --stat`.

(If Task 1 Step 4 reported a different text than `feature "vendored-protox"`, use that text here and in Step 4.)

- [ ] **Step 3: Add Guard 2 to the `ci.yml` env block**

In `.github/workflows/ci.yml`, find:

```yaml
  CARGO_PROFILE_DEV_DEBUG: line-tables-only
  NIGHTLY_TOOLCHAIN: nightly-2026-05-01
```

Replace with:

```yaml
  CARGO_PROFILE_DEV_DEBUG: line-tables-only
  NIGHTLY_TOOLCHAIN: nightly-2026-05-01
  # SMA-623: the workspace compiles the Temporal protos with protox
  # (temporalio-client's `vendored-protox` feature) and needs no system protoc.
  # This path does not exist. prost-build reads PROTOC before PATH, so a build
  # script that calls protoc fails with "Could not find `protoc`" instead of
  # silently using a protoc that a runner image happens to ship. The name has no
  # CARGO_/RUST prefix, so it is not part of the rust-cache key.
  PROTOC: /nonexistent/protoc-see-SMA-623
```

- [ ] **Step 4: Add Guard 1 to `build-no-default-features`**

In `.github/workflows/ci.yml`, find:

```yaml
            exit 1
          fi
          echo "no axum under runtime-actix"
```

Replace with:

```yaml
            exit 1
          fi
          echo "no axum under runtime-actix"
      # SMA-623: the workspace compiles the Temporal protos with protox. This
      # assertion fails if `vendored-protox` leaves temporalio-client's features
      # in the root Cargo.toml. It only resolves the graph (no build), so it does
      # not depend on the cache or on the runner image. The PROTOC guard in the
      # workflow env fires only when a build script runs; this one always runs.
      - name: Assert vendored-protox is active for the Temporal protos
        run: |
          if ! cargo tree -p paigasus-helikon-runtime-temporal -e features -i prost-wkt-types | grep -q 'feature "vendored-protox"'; then
            echo "::error::vendored-protox is not active on prost-wkt-types, so the Temporal protos need a system protoc (SMA-623). Keep \"vendored-protox\" in temporalio-client's features in the root Cargo.toml."
            exit 1
          fi
          echo "vendored-protox is active"
```

- [ ] **Step 5: Remove the six `ci.yml` setup-protoc steps**

Delete each of these blocks completely (comment lines and the `uses:` line):

`clippy` (lines 72-75):

```yaml
      # Pinned, checksum-verified protoc — temporalio-protos compiles .proto at
      # build time (prost-build); a system protoc is required (SMA-332). The
      # version and its digests live in the action (SMA-458).
      - uses: ./.github/actions/setup-protoc
```

`test` (lines 119-124):

```yaml
      # Pinned, checksum-verified protoc — temporalio-protos compiles .proto at
      # build time (prost-build); a system protoc is required (SMA-332). The
      # version and its digests live in the action (SMA-458).
      # Cross-platform: this is the only site exercising the macOS and Windows
      # branches of the action, so it carries all three pinned digests.
      - uses: ./.github/actions/setup-protoc
```

`build-no-default-features` (lines 175-178), `docs` (lines 214-217), `doc-coverage` (lines 245-248): the same 4-line block as `clippy`.

`sessions-it` (lines 304-308):

```yaml
      # Pinned, checksum-verified protoc — temporalio-protos compiles .proto at
      # build time (prost-build); a system protoc is required (SMA-332). The
      # version and its digests live in the action (SMA-458).
      - if: steps.filter.outputs.sessions == 'true'
        uses: ./.github/actions/setup-protoc
```

In the `sessions-it` path filter, find:

```yaml
              # The protoc install lives here since SMA-458. Without this entry a
              # protoc bump would skip this REQUIRED job, reporting green having
              # run nothing.
              - '.github/actions/**'
```

Replace with:

```yaml
              # Local composite actions. A change to an action that this job
              # uses must run this REQUIRED job; without this entry the job
              # would report green having run nothing.
              - '.github/actions/**'
```

- [ ] **Step 6: Edit `msrv.yml` and `integration.yml`**

`.github/workflows/msrv.yml`: delete lines 44-47 (the same 4-line block as `clippy`).

`.github/workflows/integration.yml`:

Find:

```yaml
    # Must absorb a cold build of temporalio-sdk-core + prost/tonic + protoc
    # codegen — the heaviest dependency tree in the workspace. rust-cache usually
```

Replace with:

```yaml
    # Must absorb a cold build of temporalio-sdk-core + prost/tonic + protox
    # codegen — the heaviest dependency tree in the workspace. rust-cache usually
```

Find:

```yaml
              # The protoc install lives here since SMA-458; without this entry a
              # protoc bump would skip this job entirely.
              - '.github/actions/**'
```

Replace with:

```yaml
              # Local composite actions. A change to an action that this job
              # uses must run this job.
              - '.github/actions/**'
```

Delete this block completely (lines 127-133):

```yaml
      # Pinned, checksum-verified protoc — temporalio-protos compiles .proto at
      # build time (prost-build); a system protoc is required (SMA-332). The
      # version and its digests live in the action (SMA-458). No token needed:
      # the action builds the asset URL from a hardcoded version and makes no
      # GitHub API call, so the unauthenticated API rate limit does not apply.
      - if: steps.decide.outputs.run == 'true'
        uses: ./.github/actions/setup-protoc
```

- [ ] **Step 7: Prove Guard 1 passes with the feature, and that the sites are gone**

Run the guard body exactly as the workflow does:

```bash
if ! cargo tree -p paigasus-helikon-runtime-temporal -e features -i prost-wkt-types | grep -q 'feature "vendored-protox"'; then echo "GUARD FAILS (WRONG)"; exit 1; fi; echo "vendored-protox is active"
grep -rn 'setup-protoc' .github/workflows
grep -rn 'protoc' .github/workflows
```

Expected: `vendored-protox is active`. The `setup-protoc` grep hits **only** `release-plz.yml`. The `protoc` grep hits only `release-plz.yml`, the new Guard 1/Guard 2 text in `ci.yml`, and the `protox` comment in `integration.yml` (it contains "protox", not "protoc" — confirm it does not match).

- [ ] **Step 8: Parse the edited workflows**

```bash
for f in .github/workflows/ci.yml .github/workflows/msrv.yml .github/workflows/integration.yml; do ruby -ryaml -e 'YAML.load_file(ARGV[0]); puts "ok #{ARGV[0]}"' "$f" || echo "PARSE FAIL $f"; done
ruby -ryaml -e 'y=YAML.load_file(".github/workflows/ci.yml"); puts y["env"]["PROTOC"]; puts y["jobs"]["build-no-default-features"]["steps"].map{|s| s["name"]}.compact'
```

Expected: `ok` for each file; `/nonexistent/protoc-see-SMA-623`; the step list includes `Assert no axum leakage under runtime-actix` and `Assert vendored-protox is active for the Temporal protos`.

- [ ] **Step 9: The cache-key env guard**

```bash
/opt/homebrew/bin/bash scripts/check-cargo-profile-env-sync.sh
/opt/homebrew/bin/bash scripts/check-cargo-profile-env-sync-selftest.sh
```

Expected: both pass. (`PROTOC` has no `CARGO_`/`RUST` prefix, so the sync script must ignore it. If it fails, STOP and report the output.)

- [ ] **Step 10: Commit**

```bash
git add .github/workflows/ci.yml .github/workflows/msrv.yml .github/workflows/integration.yml
git commit -m "ci(workflows): SMA-623 drop setup-protoc outside release-plz and guard protox

protox compiles the Temporal protos, so the 8 non-publishing jobs no longer
install protoc. release-plz.yml keeps it for the semver-check baseline until
SMA-687. Two ci.yml guards catch a regression: a cargo tree assertion that
vendored-protox is active, and PROTOC set to a path that does not exist.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
git show --stat HEAD
```

Expected: the commit contains exactly the three workflow files.

---

### Task 3: Documents — CONTRIBUTING, runbook, CLAUDE.md

**Files:**
- Modify: `CONTRIBUTING.md` (lines 148-160 "Build prerequisites"; line 250)
- Modify: `docs/runbooks/ci-architecture.md` (line 4 scope; lines 101-105 protoc section; line 113)
- Modify: `CLAUDE.md` (line 112; the CI section)

**Interfaces:**
- Consumes: the facts of Tasks 1-2 (feature name, guard names, `PROTOC` value, SMA-687).
- Produces: documents that do not tell anyone to install `protoc`.

- [ ] **Step 1: Prove the stale text (the "failing test")**

```bash
grep -n 'no vendored `protoc` fallback\|brew install protobuf\|like `PROTOC_VERSION`' CONTRIBUTING.md
grep -n 'at all nine sites' docs/runbooks/ci-architecture.md
```

Expected: hits in both files.

- [ ] **Step 2: Rewrite "Build prerequisites" in `CONTRIBUTING.md`**

Replace everything from the line `## Build prerequisites` up to (not including) the next `##` heading — that is, lines 148-160 — with:

````markdown
## Build prerequisites

To build the workspace, you need only the Rust toolchain (rustup; see [MSRV](#msrv) below). You do not need a system `protoc` (Protocol Buffers compiler).

`paigasus-helikon-runtime-temporal` depends on `temporalio-protos`, which compiles `.proto` files at build time. The root `Cargo.toml` enables the `vendored-protox` feature on `temporalio-client`. With this feature, `temporalio-protos` and `prost-wkt-types` (the only two crates in the graph that compile `.proto` files) use `protox`, a protobuf compiler in pure Rust, instead of a system `protoc` (SMA-623).

CI sets `PROTOC` to a path that does not exist, so a build script that calls `protoc` fails the required gates. If a local build fails with "Could not find `protoc`", a dependency has started to call `protoc`. Do not install `protoc` to make the error go away. Read the "protoc and protox" section of `docs/runbooks/ci-architecture.md` first.

````

(Keep exactly one blank line before the next `##` heading.)

- [ ] **Step 3: Correct `CONTRIBUTING.md:250`**

Find:

```markdown
**not** tracked by Dependabot — bumping it is a deliberate act, like `PROTOC_VERSION`
and `NIGHTLY_TOOLCHAIN`. `npx markdownlint-cli2 --fix` resolves most findings
```

Replace with:

```markdown
**not** tracked by Dependabot — bumping it is a deliberate act, like
`NIGHTLY_TOOLCHAIN`. `npx markdownlint-cli2 --fix` resolves most findings
```

- [ ] **Step 4: Runbook scope line**

In `docs/runbooks/ci-architecture.md`, find:

```markdown
> they are, and the incidents that shaped them (SMA-306/330/335/452/457/458/479/486/487/581/618).
```

Replace with:

```markdown
> they are, and the incidents that shaped them (SMA-306/330/335/452/457/458/479/486/487/581/618/623).
```

- [ ] **Step 5: Rewrite the runbook protoc section**

Replace the heading `## protoc (\`.github/actions/setup-protoc\`)` and its first paragraph (line 101-103) with the following. Keep the second paragraph (line 105, "**Nothing tracks the protoc pin …**") and apply the two small edits listed after this block.

```markdown
## protoc and protox

**The workspace compiles its `.proto` files with `protox`, not with a system `protoc`** (SMA-623). Only two build scripts in the dependency graph compile `.proto` files: `temporalio-protos` (through `tonic-prost-build`) and `prost-wkt-types` (through `prost-build`). The root `Cargo.toml` enables `vendored-protox` on `temporalio-client`. The feature goes through `temporalio-common` to `temporalio-protos`, which also enables it on `prost-wkt-types`. Both build scripts then call `protox::compile` and `skip_protoc_run()`. `temporalio-sdk` and `temporalio-sdk-core` do not forward the feature, so it must stay on `temporalio-client` or `temporalio-common`. SMA-623 measured the change: the generated Rust code is byte-identical to the `protoc` output (the order of the `impl` blocks in `temporalio-common`'s `payload_visitor_impl.rs` changes between any two builds, because that build script iterates a `HashSet`), and the cache cost is about 10.4 MB compressed per cache entry that compiles `runtime-temporal`. The full measurements are in `docs/superpowers/specs/2026-09-25-sma-623-vendored-protox-design.md`.

**Two guards in `ci.yml` stop a silent return to `protoc`.** Guard 1 is a step in `build-no-default-features`: `cargo tree -p paigasus-helikon-runtime-temporal -e features -i prost-wkt-types` must show `feature "vendored-protox"`. It only resolves the graph, so it does not depend on the cache or on the runner image. Guard 2 is the workflow-level `PROTOC: /nonexistent/protoc-see-SMA-623`. prost-build reads `PROTOC` before `PATH`, so any build script that calls `protoc` fails with "Could not find `protoc`" instead of using a `protoc` that a runner image ships. Guard 2 also catches a **new** dependency that calls `protoc`, which Guard 1 cannot see. Guard 2 fires only when a build script runs: the Temporal build scripts do not emit `rerun-if-env-changed=PROTOC`, so a cached build unit does not run again. The name has no `CARGO_`/`RUST` prefix, so it is not part of the rust-cache key.

**`release-plz.yml` still installs `protoc`, on purpose, until SMA-687.** release-plz runs `cargo-semver-checks` by default, and the check builds the **published** baseline. `paigasus-helikon-runtime-temporal` 0.5.0 and the facade 0.6.2 have no `vendored-protox`, so their build calls `protoc`. A PR never runs `release-plz.yml`, so a failure there appears only after a merge. SMA-687 removes the step, the action and the pin after release-plz has published a `runtime-temporal` version and a facade version that include the feature.

**Recovery, if `protox` cannot compile a future Temporal proto** (for example, protobuf editions): remove `"vendored-protox"` from `temporalio-client` in the root `Cargo.toml`, remove both guards from `ci.yml`, and add the `setup-protoc` step again at the sites that compile `runtime-temporal` (`ci.yml` `clippy`, `test`, `docs`, `doc-coverage`, and `integration.yml` `temporal-it`). The action is in `.github/actions/setup-protoc/` until SMA-687; after SMA-687, restore it from the git history. Downstream users who hit the same problem can pin `cargo update -p temporalio-protos --precise 0.9.0`.

**`.github/actions/setup-protoc`** (SMA-458) is a repo-local composite action. Since SMA-623 only `release-plz.yml` uses it. It installs **protoc 35.1**, pinned exactly and verified against a per-platform SHA-256 **before** extraction. It replaced `arduino/setup-protoc`, whose `version` input **defaults to `23.x`, not to latest** — the action's README claims otherwise and is wrong, and CI had therefore been running **23.4** since SMA-332. `install.sh` does download → verify → extract → export; that order is load-bearing, so an unverified archive never reaches an executable location. It exports `PROTOC` and `PROTOC_INCLUDE` via `$GITHUB_ENV` as well as prepending to `$GITHUB_PATH`. **`verify.sh` must stay its own step**: `$GITHUB_PATH`/`$GITHUB_ENV` writes do not affect the step that makes them. Only `Linux-X64`, `macOS-ARM64` and `Windows-X64` are supported.
```

Then, in the kept paragraph that starts `**Nothing tracks the protoc pin`, make these two edits:

- Find `**Nothing tracks the protoc pin — bumping it is a human act with no prompt.**` and replace with `**Nothing tracks the protoc pin — bumping it is a human act with no prompt.** Since SMA-623 the pin serves only `release-plz.yml`; SMA-687 removes it.`
- Find `if protobuf removes or replaces the v35.1 assets, every required job and \`release-plz\` go red until someone bumps the pin.` and replace with `if protobuf removes or replaces the v35.1 assets, \`release-plz\` goes red until someone bumps the pin.`

- [ ] **Step 6: Runbook line 113**

Find:

```markdown
alongside `TEMPORAL_CLI_VERSION` in `integration.yml` and `PROTOC_VERSION` in `.github/actions/setup-protoc/install.sh`.
```

Replace with:

```markdown
alongside `TEMPORAL_CLI_VERSION` in `integration.yml` and `PROTOC_VERSION` in `.github/actions/setup-protoc/install.sh` (used only by `release-plz.yml` since SMA-623).
```

- [ ] **Step 7: `CLAUDE.md`**

Find:

```markdown
Four pins are hand-bumped with nothing tracking them: `PROTOC_VERSION` and its three digests in `.github/actions/setup-protoc/install.sh`, `TEMPORAL_CLI_VERSION`
```

Replace with:

```markdown
Four pins are hand-bumped with nothing tracking them: `PROTOC_VERSION` and its three digests in `.github/actions/setup-protoc/install.sh` (since SMA-623 used only by `release-plz.yml`, for the semver-check baseline; SMA-687 removes it), `TEMPORAL_CLI_VERSION`
```

Then, directly after the paragraph that ends `verify upstream independently first.` (line 112), insert a new paragraph:

```markdown
**The workspace needs no system `protoc`** (SMA-623): `temporalio-client`'s `vendored-protox` feature compiles the Temporal protos with `protox`. `ci.yml` sets `PROTOC` to a path that does not exist, and `build-no-default-features` asserts that the feature is active. If a build fails with "Could not find `protoc`", do not add a `protoc` install — find which build script calls it. Rationale and recovery: `docs/runbooks/ci-architecture.md` → "protoc and protox".
```

- [ ] **Step 8: Lint**

```bash
npx markdownlint-cli2
/opt/homebrew/bin/bash scripts/check-markdownlint-config.sh
```

Expected: `Summary: 0 error(s)` and the config check passes. (Run `npm ci` once first if `node_modules` is missing.)

- [ ] **Step 9: Prove the stale text is gone**

```bash
grep -n 'no vendored `protoc` fallback\|brew install protobuf\|like `PROTOC_VERSION`' CONTRIBUTING.md; echo "exit $?"
grep -n 'at all nine sites' docs/runbooks/ci-architecture.md; echo "exit $?"
```

Expected: no hits, `exit 1` twice.

- [ ] **Step 10: Commit**

```bash
git add CONTRIBUTING.md docs/runbooks/ci-architecture.md CLAUDE.md
git commit -m "docs(contributing): SMA-623 drop the protoc prerequisite and document protox

CONTRIBUTING no longer asks for a system protoc. The CI runbook and CLAUDE.md
describe protox, the two guards, the release-plz exception until SMA-687,
and the recovery path.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
git show --stat HEAD
```

Expected: the commit contains exactly the three files.

---

### Task 4: Full local verification (spec section 8, steps 1-10)

**Files:** none are expected to change. If a step fails, fix the cause in the file that owns it, commit that fix with the type and scope of the owning task, and run the failed step again.

**Interfaces:**
- Consumes: Tasks 1-3.
- Produces: a verification report with the result of every step.

Run every command from the worktree root with `PROTOC=/nonexistent/protoc` where shown.

- [ ] **Step 1:** `PROTOC=/nonexistent/protoc cargo build --workspace --all-features` → exit 0.
- [ ] **Step 2:** `PROTOC=/nonexistent/protoc cargo test --workspace --all-features 2>&1 | grep -E '^test result|FAILED|panicked'` → every `test result` has `0 failed`. If about 48 `providers-bedrock` tests fail with `NATIVE_ROOTS` errors, that is a known macOS host artifact that depends on the checkout path. Then repeat the run in a detached worktree under the scratchpad (`git worktree add --detach <scratchpad>/verify HEAD`, run there, then `git worktree remove --force <scratchpad>/verify`) and report both results.
- [ ] **Step 3:** `cargo fmt --all -- --check` → exit 0; `PROTOC=/nonexistent/protoc cargo clippy --workspace --all-features --all-targets -- -D warnings` → exit 0.
- [ ] **Step 4:** `PROTOC=/nonexistent/protoc RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps` → exit 0.
- [ ] **Step 5:** `cargo tree -i ring --all-features -e normal --target all` → `warning: nothing to print.`
- [ ] **Step 6:** `cargo deny check` → `advisories ok, bans ok, licenses ok, sources ok`; `cargo audit --deny warnings` → exit 0. If a tool is missing, report it; do not install it.
- [ ] **Step 7:** `/opt/homebrew/bin/bash scripts/check-cargo-profile-env-sync.sh` and `/opt/homebrew/bin/bash scripts/check-cargo-profile-env-sync-selftest.sh` → both pass.
- [ ] **Step 8:** `npx markdownlint-cli2` → 0 errors; `/opt/homebrew/bin/bash scripts/check-markdownlint-config.sh` → pass.
- [ ] **Step 9:** `grep -rniE 'protoc' .github CONTRIBUTING.md CLAUDE.md docs/runbooks README.md crates/*/README.md docs/book/src` → review every hit. Allowed: `release-plz.yml`, `.github/actions/setup-protoc/*`, the two guards and the `PROTOC` comment in `ci.yml`, the new text of Task 3, and the `protox` README sentence. Any hit that tells a reader to install `protoc`, or that says a CI job other than `release-plz` installs it, is a failure.
- [ ] **Step 10:** `git diff main -- Cargo.lock | grep -E '^-version = '` → no output. `git diff main --stat` → only the files of Tasks 1-3 plus the spec and this plan.

Report every step with its command and result. Do not claim a pass without the output.

---

## After the plan (not tasks for implementers)

These belong to the pipeline stages after Stage 4 and are listed so that nothing is lost: the local CodeRabbit review (Stage 5); the PR with the title `feat(runtime-temporal): SMA-623 build without a system protoc by compiling temporal protos with protox`, a body that lists the added lockfile packages and links SMA-687 by URL (Stage 6); the CI checks in spec section 8 steps 12-13; and, after the merge, steps 14-15 (release-plz run, per-key cache sizes, a comment on SMA-623).
