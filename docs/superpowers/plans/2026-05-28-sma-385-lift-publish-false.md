# SMA-385 Lift `publish = false` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop release-plz from being permanently stuck (no version bump proposals on `feat(*)` merges) by moving the workspace from the unsupported `publish = false` mode to the supported "publish to crates.io" mode.

**Architecture:** Six real crates ship to crates.io at `0.1.0`. Seven docstring-only stubs are pre-published at `0.0.0` as name-claim placeholders, then locked with per-package `publish = false` + `release = false`. The macros↔facade dev-dep cycle that blocks `cargo publish` for `-macros` is broken by moving one trybuild test from `-macros` to the facade. The SBOM workflow's tag-glob is corrected from the dead `v*` to `paigasus-helikon-v*` so it fires on facade tags.

**Tech Stack:** Cargo workspace, release-plz, GitHub Actions, crates.io. Spec: [`docs/superpowers/specs/2026-05-28-sma-385-lift-publish-false-design.md`](../specs/2026-05-28-sma-385-lift-publish-false-design.md).

**Branch:** `feature/sma-385-release-plz-isnt-bumping-paigasus-helikon-core-past-010-on` (already created).

---

## Task 0: Preflight verification

**Purpose:** Confirm Sven's prerequisites are in place and the world looks as the spec describes. No code changes; no commits.

**Files:** none touched.

- [ ] **Step 0.1: Verify `CARGO_REGISTRY_TOKEN` repo secret is set**

This is a Sven manual action that must be done in the GitHub UI:

1. Open <https://crates.io/settings/tokens>, click "New Token".
2. Name: `paigasus-helikon-release-plz-ci`. Scopes: `publish-new`, `publish-update`. Crate scope: leave empty (broad, for first publishes). Expiration: 90 days (recommended).
3. Copy the token. crates.io shows it once.
4. Open <https://github.com/SMK1085/paigasus-helikon/settings/secrets/actions>, click "New repository secret".
5. Name: `CARGO_REGISTRY_TOKEN`. Value: the token from step 3. Click "Add secret".

Verification command (does NOT print the token, just confirms its presence):

```bash
gh secret list --repo SMK1085/paigasus-helikon | grep -E '^CARGO_REGISTRY_TOKEN\s'
```

Expected output: one line starting with `CARGO_REGISTRY_TOKEN`. If empty, halt — Sven needs to set the secret before continuing.

- [ ] **Step 0.2: Verify all 13 names are available on crates.io**

```bash
for name in paigasus-helikon paigasus-helikon-core paigasus-helikon-macros \
            paigasus-helikon-providers-openai paigasus-helikon-providers-anthropic \
            paigasus-helikon-sessions-sqlite \
            paigasus-helikon-mcp paigasus-helikon-tools paigasus-helikon-evals \
            paigasus-helikon-runtime-tokio paigasus-helikon-runtime-axum \
            paigasus-helikon-runtime-temporal paigasus-helikon-runtime-agentcore; do
  status=$(curl -s -o /dev/null -w '%{http_code}' "https://crates.io/api/v1/crates/$name")
  printf '%-44s -> %s\n' "$name" "$status"
done
```

Expected output: thirteen `404` lines (one per name). If any line shows `200`, halt — the name is taken; investigate before continuing.

- [ ] **Step 0.3: Baseline `cargo publish --dry-run` for all 13 crates**

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

Expected results (matching the spec's §4.3 table):

| Crate | Result |
|---|---|
| `paigasus-helikon-core` | ✅ ends with `warning: aborting upload due to dry run` |
| `paigasus-helikon-macros` | ❌ `no matching package named 'paigasus-helikon' found` |
| `paigasus-helikon-providers-openai` | ❌ `no matching package named 'paigasus-helikon-core' found` |
| `paigasus-helikon-providers-anthropic` | ❌ `no matching package named 'paigasus-helikon-core' found` |
| `paigasus-helikon-sessions-sqlite` | ❌ `no matching package named 'paigasus-helikon-core' found` |
| `paigasus-helikon` (facade) | ❌ `no matching package named 'paigasus-helikon-core' found` |
| 7 stubs | ✅ all end with `warning: aborting upload due to dry run` |

Any deviation halts the plan. The expected failures resolve in Tasks 1, 2, and the post-merge CI publish; the expected passes confirm the §4.4 publish can proceed.

---

## Task 1: Break the macros↔facade dev-dep cycle

**Purpose:** Move the `facade_only_consumer.rs` trybuild test from `-macros` to the facade so `-macros` no longer has a publish-time dep on the facade. Without this, `cargo publish -p paigasus-helikon-macros` fails with `no matching package named 'paigasus-helikon' found`.

**Files:**
- Create: `crates/paigasus-helikon/tests/trybuild.rs`
- Create: `crates/paigasus-helikon/tests/ui/facade_only_consumer.rs`
- Modify: `crates/paigasus-helikon/Cargo.toml` (extend `[dev-dependencies]`)
- Delete: `crates/paigasus-helikon-macros/tests/ui/facade_only_consumer.rs`
- Modify: `crates/paigasus-helikon-macros/tests/trybuild.rs` (remove one line)
- Modify: `crates/paigasus-helikon-macros/Cargo.toml` (drop facade dev-dep)

- [ ] **Step 1.1: Create the facade trybuild runner**

Create `crates/paigasus-helikon/tests/trybuild.rs` with this exact content:

```rust
//! UI tests that pin the macro's facade-path resolution.
//!
//! See SMA-385 spec §5.5 — this test lives here (and not in
//! `paigasus-helikon-macros`) because keeping the dev-dep cycle
//! macros→facade blocks `cargo publish -p paigasus-helikon-macros`.

#[test]
fn trybuild_ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/facade_only_consumer.rs");
}
```

- [ ] **Step 1.2: Create the moved test file in the facade**

Create `crates/paigasus-helikon/tests/ui/facade_only_consumer.rs` with this exact content (copied verbatim from `crates/paigasus-helikon-macros/tests/ui/facade_only_consumer.rs`):

```rust
//! Compile-pass: this file mentions only `paigasus_helikon` (the
//! facade), never `paigasus_helikon_core` directly. It locks the
//! proc-macro-crate auto-resolution: when only the facade is in the
//! dep graph, the macro must emit paths rooted at
//! `::paigasus_helikon::core::…`.

use std::sync::Arc;

use paigasus_helikon::core::{Tool, ToolContext, ToolError};
use paigasus_helikon::{tool, tools};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

struct MyCtx;

#[derive(Deserialize, JsonSchema)]
struct AddArgs {
    a: i64,
    b: i64,
}

#[derive(Serialize, JsonSchema)]
struct AddOut {
    sum: i64,
}

/// Adds two numbers.
#[tool]
async fn add(
    _ctx: &ToolContext<MyCtx>,
    args: AddArgs,
) -> Result<AddOut, ToolError> {
    Ok(AddOut { sum: args.a + args.b })
}

fn main() {
    let _r: Vec<Arc<dyn Tool<MyCtx>>> = tools![add];
}
```

- [ ] **Step 1.3: Extend the facade's `[dev-dependencies]`**

Modify `crates/paigasus-helikon/Cargo.toml`. The current `[dev-dependencies]` block is:

```toml
[dev-dependencies]
# Installs git hooks from .cargo-husky/hooks/ when this crate's
# dev-deps are realized (e.g. `cargo test -p paigasus-helikon --no-run`).
# See SMA-335 design doc §4.
cargo-husky = { version = "1", default-features = false, features = ["user-hooks"] }
```

Replace it with:

```toml
[dev-dependencies]
# Installs git hooks from .cargo-husky/hooks/ when this crate's
# dev-deps are realized (e.g. `cargo test -p paigasus-helikon --no-run`).
# See SMA-335 design doc §4.
cargo-husky = { version = "1", default-features = false, features = ["user-hooks"] }
# Trybuild for the facade_only_consumer UI test (SMA-385 §5.5 — moved
# here from paigasus-helikon-macros to break the publish-time dep cycle).
trybuild    = { workspace = true }
schemars    = { workspace = true }
serde       = { workspace = true }
```

- [ ] **Step 1.4: Verify the moved test compiles and passes in the facade**

Run:

```bash
cargo test -p paigasus-helikon --features macros trybuild_ui -- --test-threads=1
```

Expected: ends with `test trybuild_ui ... ok` and `test result: ok. 1 passed; 0 failed`. If it fails on a missing `tool!` / `tools!` macro, double-check the `--features macros` flag.

- [ ] **Step 1.5: Delete the source test file from `-macros`**

```bash
rm crates/paigasus-helikon-macros/tests/ui/facade_only_consumer.rs
```

- [ ] **Step 1.6: Remove the facade-only line from the macros trybuild runner**

Modify `crates/paigasus-helikon-macros/tests/trybuild.rs`. The current content is:

```rust
//! UI tests for #[tool] and tools!. The workflow restricts execution to
//! the latest-stable CI matrix row (`.github/workflows/ci.yml`) because
//! trybuild `.stderr` snapshots pin rustc diagnostic text byte-for-byte
//! and that text drifts across rustc releases.

#[test]
fn trybuild_ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/bad_*.rs");
    t.compile_fail("tests/ui/no_description.rs");
    t.compile_fail("tests/ui/empty_description.rs");
    t.pass("tests/ui/facade_only_consumer.rs");
}
```

Delete the `t.pass(…)` line so it reads:

```rust
//! UI tests for #[tool] and tools!. The workflow restricts execution to
//! the latest-stable CI matrix row (`.github/workflows/ci.yml`) because
//! trybuild `.stderr` snapshots pin rustc diagnostic text byte-for-byte
//! and that text drifts across rustc releases.

#[test]
fn trybuild_ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/bad_*.rs");
    t.compile_fail("tests/ui/no_description.rs");
    t.compile_fail("tests/ui/empty_description.rs");
}
```

- [ ] **Step 1.7: Drop the facade dev-dep from `-macros/Cargo.toml`**

Modify `crates/paigasus-helikon-macros/Cargo.toml`. The current `[dev-dependencies]` block contains:

```toml
[dev-dependencies]
paigasus-helikon-core = { workspace = true }
paigasus-helikon      = { workspace = true, features = ["macros"] }
async-trait  = { workspace = true }
schemars     = { workspace = true }
serde        = { workspace = true }
serde_json   = { workspace = true }
tokio        = { workspace = true, features = ["macros", "rt"] }
anyhow       = { workspace = true }
trybuild     = { workspace = true }
insta        = { workspace = true, features = ["json"] }
```

Delete only the `paigasus-helikon = …` line. The result:

```toml
[dev-dependencies]
paigasus-helikon-core = { workspace = true }
async-trait  = { workspace = true }
schemars     = { workspace = true }
serde        = { workspace = true }
serde_json   = { workspace = true }
tokio        = { workspace = true, features = ["macros", "rt"] }
anyhow       = { workspace = true }
trybuild     = { workspace = true }
insta        = { workspace = true, features = ["json"] }
```

- [ ] **Step 1.8: Verify macros tests still pass without the facade dev-dep**

Run:

```bash
cargo test -p paigasus-helikon-macros
```

Expected: all tests pass. Look for `test result: ok` on every line. If any test fails on `paigasus_helikon::…` imports, that test also needs migration — but the spec verified only `facade_only_consumer.rs` uses the facade, so this should be clean.

- [ ] **Step 1.9: Verify the cycle is broken via dry-run**

Run:

```bash
cargo publish --dry-run -p paigasus-helikon-macros --allow-dirty 2>&1 | tail -8
```

Expected: now fails with `no matching package named 'paigasus-helikon-core' found` (the ordering issue that release-plz fixes post-merge by publishing core first), NOT with `no matching package named 'paigasus-helikon' found` (the cycle). If the cycle message still appears, the dev-dep wasn't removed cleanly — re-check Step 1.7.

- [ ] **Step 1.10: Run local CI gates**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
```

Expected: all three pass clean. If clippy complains about unused imports in `paigasus-helikon-macros` (since the facade dev-dep was removed), check whether anything in `tests/` was depending on it transitively.

- [ ] **Step 1.11: Commit**

```bash
git add crates/paigasus-helikon/tests/ \
        crates/paigasus-helikon/Cargo.toml \
        crates/paigasus-helikon-macros/tests/trybuild.rs \
        crates/paigasus-helikon-macros/Cargo.toml
git rm crates/paigasus-helikon-macros/tests/ui/facade_only_consumer.rs
git commit -m "$(cat <<'EOF'
refactor(workspace): SMA-385 move facade_only_consumer trybuild test to facade

The macros crate had a [dev-dependencies] entry on the facade
(paigasus-helikon = { workspace = true, features = ["macros"] }) to
support a single trybuild test that locked in the proc-macro-crate
facade-path resolution. cargo's publish resolver runs at upload time
and treats unresolved dev-deps as a fatal error, so this dev-dep made
paigasus-helikon-macros unpublishable as long as the facade wasn't on
crates.io — and the facade depends on macros (optional regular dep),
which created a publish-time cycle.

Move the test from crates/paigasus-helikon-macros/tests/ui/ to
crates/paigasus-helikon/tests/ui/. Add a new trybuild runner in the
facade. Extend the facade's [dev-dependencies] with trybuild +
schemars + serde so the test compiles. Drop the facade dev-dep from
the macros crate. Coverage of the facade-path resolution is preserved
— the test still runs under `cargo test --workspace --all-features`.

See spec §5.5.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Pre-publish 7 stubs at `0.0.0`

**Purpose:** Land `0.0.0` placeholder versions of each stub on crates.io so the facade's optional deps resolve when it publishes. Must happen *before* Task 3 adds `publish = false` to the stubs.

**Files:** none touched locally. This step mutates crates.io.

**Sven manual action — cannot be automated by a subagent.** crates.io publishes are irreversible (the version stays even if yanked).

- [ ] **Step 2.1: Confirm `cargo login` is set up locally**

```bash
cargo login --help > /dev/null && test -f ~/.cargo/credentials.toml && echo "credentials present"
```

Expected: `credentials present`. If missing, run `cargo login <token>` with the same token added to GH secrets in Step 0.1 (or a separate short-lived publish-new token).

- [ ] **Step 2.2: Publish each of the 7 stubs at `0.0.0`**

```bash
for stub in paigasus-helikon-mcp paigasus-helikon-tools paigasus-helikon-evals \
            paigasus-helikon-runtime-tokio paigasus-helikon-runtime-axum \
            paigasus-helikon-runtime-temporal paigasus-helikon-runtime-agentcore; do
  echo "=== publishing $stub ==="
  cargo publish -p "$stub"
  echo "sleeping 30s for index refresh"
  sleep 30
done
```

Expected: each run ends with `Uploading <stub> v0.0.0 (...)` and no error. If a run fails partway through, individual stubs can be retried with `cargo publish -p <stub>` — they have no internal deps so order doesn't matter beyond the index-refresh delay.

- [ ] **Step 2.3: Verify all 7 stubs are visible on crates.io**

```bash
for stub in paigasus-helikon-mcp paigasus-helikon-tools paigasus-helikon-evals \
            paigasus-helikon-runtime-tokio paigasus-helikon-runtime-axum \
            paigasus-helikon-runtime-temporal paigasus-helikon-runtime-agentcore; do
  status=$(curl -s -o /dev/null -w '%{http_code}' "https://crates.io/api/v1/crates/$stub")
  printf '%-44s -> %s\n' "$stub" "$status"
done
```

Expected: seven `200` lines (one per stub). If any is still `404`, the index hasn't refreshed yet — wait 30s and retry.

**No commit for this task** (no repo changes). Continue to Task 3.

---

## Task 3: Add `publish = false` to 7 stubs and CLI

**Purpose:** Lock the stubs and CLI against accidental ad-hoc `cargo publish`. After Task 2, the stubs are on crates.io at `0.0.0`; this step ensures no one accidentally republishes them.

**Files:**
- Modify: `crates/paigasus-helikon-mcp/Cargo.toml`
- Modify: `crates/paigasus-helikon-tools/Cargo.toml`
- Modify: `crates/paigasus-helikon-evals/Cargo.toml`
- Modify: `crates/paigasus-helikon-runtime-tokio/Cargo.toml`
- Modify: `crates/paigasus-helikon-runtime-axum/Cargo.toml`
- Modify: `crates/paigasus-helikon-runtime-temporal/Cargo.toml`
- Modify: `crates/paigasus-helikon-runtime-agentcore/Cargo.toml`
- Modify: `crates/paigasus-helikon-cli/Cargo.toml`

- [ ] **Step 3.1: Add `publish = false` to each stub Cargo.toml**

For each of the 7 stubs (`-mcp`, `-tools`, `-evals`, `-runtime-tokio`, `-runtime-axum`, `-runtime-temporal`, `-runtime-agentcore`), insert a `publish = false` line after `categories.workspace = true`. Example for `-mcp` (others are identical except for `name` and `description`):

Original `crates/paigasus-helikon-mcp/Cargo.toml`:

```toml
[package]
name        = "paigasus-helikon-mcp"
description = "MCP client and server integration for the Paigasus Helikon AI SDK."
version                = "0.0.0"
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true

[lints]
workspace = true
```

After:

```toml
[package]
name        = "paigasus-helikon-mcp"
description = "MCP client and server integration for the Paigasus Helikon AI SDK."
version                = "0.0.0"
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true
publish                = false

[lints]
workspace = true
```

Repeat the single-line insertion in all 7 stub Cargo.tomls.

- [ ] **Step 3.2: Add `publish = false` to the CLI Cargo.toml**

Modify `crates/paigasus-helikon-cli/Cargo.toml`. Original:

```toml
[package]
name     = "paigasus-helikon-cli"
description = "CLI binaries (helikon, paigasus-helikon) for the Paigasus Helikon AI SDK."
autobins = false
version                = "0.0.0"
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true

[[bin]]
```

After:

```toml
[package]
name     = "paigasus-helikon-cli"
description = "CLI binaries (helikon, paigasus-helikon) for the Paigasus Helikon AI SDK."
autobins = false
version                = "0.0.0"
edition.workspace      = true
rust-version.workspace = true
authors.workspace      = true
license.workspace      = true
repository.workspace   = true
homepage.workspace     = true
keywords.workspace     = true
categories.workspace   = true
publish                = false

[[bin]]
```

- [ ] **Step 3.3: Verify the workspace still builds**

```bash
cargo build --workspace --all-features
```

Expected: clean build. `publish = false` doesn't affect compilation; this is a sanity check.

- [ ] **Step 3.4: Verify cargo now refuses to publish stubs**

```bash
cargo publish --dry-run -p paigasus-helikon-mcp --allow-dirty 2>&1 | tail -3
```

Expected: `error: crates cannot be published with publish=false` (or similar refusal). If the dry-run still claims success, the `publish = false` wasn't written correctly.

- [ ] **Step 3.5: Commit**

```bash
git add crates/paigasus-helikon-mcp/Cargo.toml \
        crates/paigasus-helikon-tools/Cargo.toml \
        crates/paigasus-helikon-evals/Cargo.toml \
        crates/paigasus-helikon-runtime-tokio/Cargo.toml \
        crates/paigasus-helikon-runtime-axum/Cargo.toml \
        crates/paigasus-helikon-runtime-temporal/Cargo.toml \
        crates/paigasus-helikon-runtime-agentcore/Cargo.toml \
        crates/paigasus-helikon-cli/Cargo.toml
git commit -m "$(cat <<'EOF'
chore(workspace): SMA-385 lock stubs and CLI from accidental cargo publish

Add `publish = false` under `[package]` in each of the 7 stub
Cargo.tomls (-mcp, -tools, -evals, -runtime-{tokio,axum,temporal,
agentcore}) and the -cli Cargo.toml. The stubs are pre-published at
0.0.0 as name-claim placeholders (Task 2 of the SMA-385 plan); this
flag prevents a re-publish of the same 0.0.0 version. The CLI is
binary-only and was already publish=false in release-plz.toml; this
adds parity at the Cargo.toml level.

When a stub later ascends to real API, follow the 4-step recipe in
CLAUDE.md (bump Cargo.toml version, drop publish=false here, drop
release=false in release-plz.toml, land as chore(release)).

See spec §5.2, §5.4.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Update `release-plz.toml`

**Purpose:** Remove the workspace-level `publish = false` (the dead-end that motivated SMA-385) and add per-package `release = false` blocks for each stub so release-plz ignores them entirely.

**Files:**
- Modify: `release-plz.toml`

- [ ] **Step 4.1: Replace `release-plz.toml` with the Stage-1 shape**

Overwrite `release-plz.toml` with this exact content (using Write since the file structure changes significantly):

```toml
# release-plz.toml — automated versioning, changelogs, GitHub releases.
# Reference: https://release-plz.dev/docs/config

[workspace]
# Bump dependent crates whenever a dep's version changes. The facade
# (paigasus-helikon) depends on every sibling via `[workspace.dependencies]`,
# so a `feat(core):` commit triggers: paigasus-helikon-core bumped →
# facade's `[workspace.dependencies].paigasus-helikon-core` line updated →
# facade itself gets a patch bump.
dependencies_update = true

# Stage 1 (SMA-385) is live: the six real crates (-core, facade, -macros,
# -providers-openai, -providers-anthropic, -sessions-sqlite) publish to
# crates.io. The seven stubs were pre-published once at 0.0.0 as name-claim
# placeholders and carry per-package overrides below. See CLAUDE.md for
# the 4-step recipe a stub follows when it ascends to real API.

[changelog]
sort_commits = "newest"

# Binary-only — never published as a library.
[[package]]
name = "paigasus-helikon-cli"
publish = false

# Stubs: pre-published at 0.0.0 as name-claim placeholders. publish=false
# in each Cargo.toml keeps cargo from re-publishing them; release=false
# here keeps release-plz from proposing bumps or CHANGELOG churn. The two
# flags MUST move together — see CLAUDE.md for the 4-step ascend recipe.
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

- [ ] **Step 4.2: Verify the TOML is parseable**

```bash
cargo build --workspace --all-features 2>&1 | tail -5
```

Expected: clean build (cargo doesn't validate release-plz.toml, but the unchanged build is the smoke test the workspace hasn't broken).

Then, if `release-plz` is installed locally, run:

```bash
release-plz update --dry-run --allow-dirty -p paigasus-helikon-core 2>&1 | tail -5
```

Expected: no parse error against `release-plz.toml`. Don't trust the bump output (we're not on main yet); just confirm no schema complaint.

- [ ] **Step 4.3: Commit**

```bash
git add release-plz.toml
git commit -m "$(cat <<'EOF'
chore(release): SMA-385 lift workspace publish=false; add stub overrides

Stage 1 of the original SMA-307 plan: real crates publish to crates.io.

Remove the workspace-level `publish = false` that release-plz can't work
around (upstream issue #2479 — non-registry mode is unsupported). Add
`[[package]] publish = false + release = false` blocks for each of the
7 stub crates so release-plz ignores them entirely. Keep the existing
CLI override.

Comments above each block point to CLAUDE.md as the single source of
truth for the 4-step ascend recipe.

See spec §5.1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Fix the SBOM workflow tag glob

**Purpose:** Change `tags: ["v*"]` to `tags: ["paigasus-helikon-v*"]` so the SBOM workflow fires on the facade's release-plz tags. The current glob has been dead since SMA-307 landed (release-plz emits `<crate>-v*`, none of which match `v*`).

**Files:**
- Modify: `.github/workflows/sbom.yml`

- [ ] **Step 5.1: Edit the trigger glob**

Modify `.github/workflows/sbom.yml`. The current header is:

```yaml
name: sbom

on:
  push:
    tags:
      - "v*"
```

Change to:

```yaml
name: sbom

on:
  push:
    tags:
      - "paigasus-helikon-v*"
```

Glob anchoring: `paigasus-helikon-v*` matches `paigasus-helikon-v0.1.0` (facade) but NOT `paigasus-helikon-core-v0.1.0` (because the char after the second `-` is `c`, not `v`). The facade gets a patch bump on every internal-dep version change via `dependencies_update = true`, so this captures every workspace state change as exactly one SBOM.

- [ ] **Step 5.2: Sanity-check the workflow file**

```bash
gh workflow view sbom.yml --repo SMK1085/paigasus-helikon 2>&1 | head -10
```

Expected: workflow definition shows. Note `gh workflow view` reads from the default branch, so the locally-edited file isn't reflected here yet — this just confirms the file name is right.

A stricter local check: pipe the file through `yq` or `python -c 'import yaml; yaml.safe_load(open("..."))'` if available, but the change is so small (one string) that visual review suffices.

- [ ] **Step 5.3: Commit**

```bash
git add .github/workflows/sbom.yml
git commit -m "$(cat <<'EOF'
ci(workflows): SMA-385 fix SBOM tag-glob to fire on facade tags

The trigger `tags: ["v*"]` (since SMA-307) never matched any tag
release-plz emits, because release-plz uses crate-prefixed tags
like `paigasus-helikon-core-v0.2.0` and `paigasus-helikon-v0.1.1`.

Change to `tags: ["paigasus-helikon-v*"]` so the SBOM workflow fires
on facade tags only. The facade gets a patch bump on every
internal-dep version change via `dependencies_update = true`, so
every workspace state change produces exactly one SBOM — cleaner
than firing on every per-crate tag.

The glob's anchoring is precise: `paigasus-helikon-v*` matches
`paigasus-helikon-v0.1.1` but NOT `paigasus-helikon-core-v0.1.0`
(the char after the second `-` is `c`, not `v`).

See spec §5.6.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Update `CLAUDE.md`

**Purpose:** Reflect that Stage 1 has happened: the six real crates are on crates.io, the seven stubs are 0.0.0 placeholders, and a new 4-step recipe replaces the legacy 0.0.0-trap manual escape.

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 6.1: Update the "Implementation status" paragraph**

Modify `CLAUDE.md`. Find this line (around line 37):

```
**Implementation status** (as of 2026-05-26): `paigasus-helikon-core`, `paigasus-helikon-macros`, `paigasus-helikon-providers-openai`, and `paigasus-helikon-providers-anthropic` carry real implementations (SMA-312/313/314/315/316/317). The remaining crates (`-mcp`, `-tools`, `-evals`, `-runtime-{tokio,axum,temporal,agentcore}`, `-cli`) are stubs that print docstrings only — real implementations land in subsequent SMA-* tickets.
```

Replace with:

```
**Implementation status** (as of 2026-05-28): `paigasus-helikon-core`, `paigasus-helikon`, `paigasus-helikon-macros`, `paigasus-helikon-providers-openai`, `paigasus-helikon-providers-anthropic`, and `paigasus-helikon-sessions-sqlite` carry real implementations (SMA-312/313/314/315/316/317/318/319) and are published to crates.io at `0.1.0` (SMA-385). The seven `-mcp`, `-tools`, `-evals`, `-runtime-{tokio,axum,temporal,agentcore}` crates are docstring-only stubs pre-published at `0.0.0` as name-claim placeholders with `publish = false` + `release = false` — real implementations land in subsequent SMA-* tickets via the 4-step ascend recipe below. `paigasus-helikon-cli` is binary-only and never published as a library.
```

- [ ] **Step 6.2: Replace the 0.0.0-trap-escape recipe with the 4-step version**

Find this block in `CLAUDE.md` (around lines 41-46):

```
**Per-crate version is the one exception**, with a two-state lifecycle:

1. **Stub state — `version = "0.0.0"`** (the default; root `[workspace.package].version` is `"0.0.0"` as a safety net). New stub crates start here so release-plz won't compute a bump until real public API ships.
2. **Released state — `version = "0.1.0"`** after the first real public-API ticket lands. The bump is its own commit, type `chore(release):`, message pattern `chore(release): SMA-XXX [bump <crate> to 0.1.0 |] escape release-plz 0.0.0 trap [for <crate>]`. **Why explicitly:** release-plz's first run created `v0.0.0` git tags for every crate (PR #6). With the tag in place, a starting version of `0.0.0` reads as "already published, nothing to do" and release-plz refuses to propose a bump no matter how many feat commits land. The manual 0.0.0 → 0.1.0 nudge in a follow-up `chore(release)` commit escapes that. SMA-347 (core + facade), SMA-350 (macros), SMA-372 (providers-openai), and SMA-317 (providers-anthropic, bundled with the impl PR) all followed this pattern. CR may flag this as "violating the `0.0.0` rule" — point them here.

Crates currently at `0.1.0`: `paigasus-helikon-core`, `paigasus-helikon`, `paigasus-helikon-macros`, `paigasus-helikon-providers-openai`, `paigasus-helikon-providers-anthropic`. Everything else stays at `0.0.0` until it ships real API.
```

Replace with:

```
**Per-crate version is the one exception**, with a two-state lifecycle:

1. **Stub state — `version = "0.0.0"` + `publish = false` in Cargo.toml + `release = false` block in `release-plz.toml`.** Every stub was pre-published once to crates.io at `0.0.0` during SMA-385 to claim the name and satisfy the facade's optional-dep resolver. After that pre-publish, cargo refuses to republish (the per-crate `publish = false`); release-plz ignores them entirely (the `release = false`).
2. **Released state — bumped to a real version (≥ `0.1.0`)** after the first real public-API ticket lands. The 4-step ascend recipe:
   - Bump `version = "0.0.0"` → `"0.1.0"` in the crate's `Cargo.toml`.
   - Remove `publish = false` from that `Cargo.toml`.
   - Remove the crate's `[[package]] … release = false` block from `release-plz.toml`.
   - Land as one `chore(release): SMA-### lift stage-1 gates for <crate>` commit on the feature branch alongside the implementation. release-plz handles the first crates.io publish on CI.

   The 4-step recipe applies to **stubs ascending from `0.0.0`**. The six already-released crates (`-core`, facade, `-macros`, `-providers-openai`, `-providers-anthropic`, `-sessions-sqlite`) ship through release-plz's normal flow — no manual ritual needed for their future bumps. The historical chain of `chore(release): … escape release-plz 0.0.0 trap …` commits in the git log (SMA-317/347/350/372/382) is pre-Stage-1 archaeology and won't recur.

Crates currently at `0.1.0` on crates.io: `paigasus-helikon-core`, `paigasus-helikon`, `paigasus-helikon-macros`, `paigasus-helikon-providers-openai`, `paigasus-helikon-providers-anthropic`, `paigasus-helikon-sessions-sqlite`. Stubs at `0.0.0` on crates.io: `paigasus-helikon-mcp`, `paigasus-helikon-tools`, `paigasus-helikon-evals`, `paigasus-helikon-runtime-tokio`, `paigasus-helikon-runtime-axum`, `paigasus-helikon-runtime-temporal`, `paigasus-helikon-runtime-agentcore`. `paigasus-helikon-cli` is binary-only and never published.
```

- [ ] **Step 6.3: Sanity-check `CLAUDE.md` renders as Markdown**

```bash
head -50 CLAUDE.md && wc -l CLAUDE.md
```

Expected: clean Markdown output (no malformed code fences, no stray characters). Verify the lines around the edits visually.

- [ ] **Step 6.4: Commit**

```bash
git add CLAUDE.md
git commit -m "$(cat <<'EOF'
docs(claude): SMA-385 document Stage 1 and the 4-step ascend recipe

Update the "Implementation status" paragraph to reflect that six real
crates are published to crates.io at 0.1.0 and seven stubs hold 0.0.0
name-claim placeholders.

Replace the legacy 0.0.0-trap-escape recipe (a 3-step manual nudge to
work around release-plz's behavior with publish=false workspaces) with
the new 4-step ascend recipe for stubs that ship real API: bump
version, drop publish=false in Cargo.toml, drop release=false in
release-plz.toml, land as chore(release). release-plz handles the
first publish on CI.

Clarify that already-released crates use release-plz's normal flow
and won't need the ascend recipe. The historical SMA-317/347/350/372/382
escape commits are pre-Stage-1 archaeology.

See spec §5.3.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Final local verification + push + PR

**Purpose:** One last full-CI-locally sanity check, then push the branch and open the PR.

**Files:** none touched.

- [ ] **Step 7.1: Run the full local CI gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: all four pass clean. If any fails, fix the underlying issue and add a follow-up commit (do NOT amend).

- [ ] **Step 7.2: Re-run dry-runs to confirm the planned end-state**

```bash
for crate in paigasus-helikon-core paigasus-helikon-macros \
             paigasus-helikon-providers-openai paigasus-helikon-providers-anthropic \
             paigasus-helikon-sessions-sqlite paigasus-helikon; do
  echo "=== $crate ==="
  cargo publish --dry-run -p "$crate" --allow-dirty 2>&1 | tail -4
done
```

Expected:
- `paigasus-helikon-core` passes (no internal deps).
- `paigasus-helikon-macros` fails on `paigasus-helikon-core` (the ordering issue — release-plz publishes core first post-merge).
- The other 3 fail on `paigasus-helikon-core` (same).
- `paigasus-helikon` (facade) fails on `paigasus-helikon-core`. With stubs already on crates.io at 0.0.0 (Task 2), the facade's stub deps resolve — the only remaining blocker is core, which release-plz publishes first.

If the facade now fails on a stub (`no matching package named 'paigasus-helikon-mcp'`), Task 2's pre-publish didn't complete — re-verify Step 2.3.

- [ ] **Step 7.3: Review the branch commit history**

```bash
git log --oneline origin/main..HEAD
```

Expected output (in this order):

```
<sha> docs(claude): SMA-385 document Stage 1 and the 4-step ascend recipe
<sha> ci(workflows): SMA-385 fix SBOM tag-glob to fire on facade tags
<sha> chore(release): SMA-385 lift workspace publish=false; add stub overrides
<sha> chore(workspace): SMA-385 lock stubs and CLI from accidental cargo publish
<sha> refactor(workspace): SMA-385 move facade_only_consumer trybuild test to facade
<sha> docs(spec): SMA-385 tighten CI token scopes to least-privilege
<sha> docs(spec): SMA-385 incorporate review feedback + dry-run findings
<sha> docs(spec): SMA-385 add design for lifting publish=false
```

(Three docs(spec) commits are the design doc + revisions from brainstorming; five code/config commits implement Tasks 1, 3, 4, 5, 6.)

- [ ] **Step 7.4: Push the branch**

```bash
git push -u origin feature/sma-385-release-plz-isnt-bumping-paigasus-helikon-core-past-010-on
```

Expected: pre-push hook runs `cargo fmt --check`, `cargo clippy`, and `convco check` against `origin/main..HEAD`. All must pass. If `convco check` flags a commit message, fix on a new commit (don't amend).

- [ ] **Step 7.5: Open the PR**

```bash
gh pr create \
  --base main \
  --head feature/sma-385-release-plz-isnt-bumping-paigasus-helikon-core-past-010-on \
  --title "chore(release): SMA-385 lift workspace publish=false and publish to crates.io" \
  --body "$(cat <<'EOF'
## Summary

- Lifts the workspace-level `publish = false` from `release-plz.toml`, moving the workspace to the supported registry mode (root cause: release-plz upstream-confirmed-unsupported in non-registry mode; symptom: `paigasus-helikon-core` stayed at 0.1.0 across SMA-318 and SMA-319 feat(core) merges).
- Pre-publishes the 7 docstring-only stubs (`-mcp`, `-tools`, `-evals`, `-runtime-{tokio,axum,temporal,agentcore}`) to crates.io at 0.0.0 as name-claim placeholders.
- Adds per-package `publish = false` to each stub Cargo.toml and the CLI Cargo.toml for defense-in-depth.
- Adds `[[package]] release = false` entries for each stub in `release-plz.toml` so release-plz ignores them entirely.
- Breaks the macros↔facade dev-dep cycle by moving the `facade_only_consumer.rs` trybuild test from `-macros` to the facade.
- Fixes the SBOM workflow's tag glob from the dead `v*` to `paigasus-helikon-v*`.
- Updates CLAUDE.md to reflect Stage 1 and the new 4-step ascend recipe.

Design: `docs/superpowers/specs/2026-05-28-sma-385-lift-publish-false-design.md`.
Plan: `docs/superpowers/plans/2026-05-28-sma-385-lift-publish-false.md`.

## Test plan

- [ ] All CI required checks (fmt, clippy, test, docs, doc-coverage, audit, deny, commits, pr-title) pass.
- [ ] After merge, release-plz publishes the 6 real crates to crates.io at 0.1.0 (Checkpoint B in the spec's §7).
- [ ] After merge, release-plz opens a follow-up `chore: release` PR proposing `paigasus-helikon-core 0.1.0 → 0.2.0` with the SMA-318 and SMA-319 entries in the CHANGELOG.
- [ ] Merging the follow-up PR triggers the SBOM workflow on the `paigasus-helikon-v0.1.1` tag push (Checkpoint C).

Resolves SMA-385.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: PR URL printed. The pr-title check on the PR enforces lowercase subject after the SMA-### prefix (per CLAUDE.md). The title above leads with `chore(release):` (valid Conventional Commits type) and a lowercase verb `lift`, so it passes both rules.

---

## Task 8: Post-merge verification (Checkpoints A–D)

**Purpose:** After the PR merges, watch release-plz CI and verify each Checkpoint in the spec's §7.

**Files:** none touched. This is read-only verification.

- [ ] **Step 8.1: Wait for the SMA-385 PR's checks to pass, then merge**

After all required checks pass on the PR, squash-merge it. The squash commit subject will match the PR title (per repo convention).

- [ ] **Step 8.2: Watch the post-merge release-plz workflow run**

```bash
sleep 10  # give GH a moment to schedule
RUN_ID=$(gh run list --workflow=release-plz.yml --branch=main --limit=1 --json databaseId --jq '.[0].databaseId')
gh run watch "$RUN_ID" --exit-status
```

Expected: workflow completes successfully (`Run completed with result: success`). If the `release` step fails on a `cargo publish` error, jump to the §6 fallback in the spec (Sven runs `cargo publish -p <crate>` locally for any missed crates).

- [ ] **Step 8.3: Checkpoint B — verify 6 real crates on crates.io at 0.1.0**

```bash
for crate in paigasus-helikon-core paigasus-helikon-macros \
             paigasus-helikon-providers-openai paigasus-helikon-providers-anthropic \
             paigasus-helikon-sessions-sqlite paigasus-helikon; do
  curl -s "https://crates.io/api/v1/crates/$crate" | jq -r '.crate.max_version'
done
```

Expected: six `0.1.0` lines. If any returns `null` or the wrong version, the publish for that crate failed — check the workflow logs.

- [ ] **Step 8.4: Checkpoint B continued — verify the follow-up `chore: release` PR opened**

```bash
gh pr list --repo SMK1085/paigasus-helikon --state open --search 'in:title chore release' --json number,title,headRefName
```

Expected: one open PR titled like `chore: release`. Open it and verify the diff proposes `paigasus-helikon-core 0.1.0 → 0.2.0` with the SMA-318 and SMA-319 CHANGELOG entries, and `paigasus-helikon 0.1.0 → 0.1.1` (the facade's patch bump via `dependencies_update`).

If no PR opens within ~5 minutes after merge, check the release-plz workflow logs:

```bash
gh run view "$RUN_ID" --log | grep -E 'release_pr_output|WARN|ERROR' | tail -20
```

If you see `release_pr_output: {"prs":[]}`, the fix didn't take — diagnose using §2 of the spec.

- [ ] **Step 8.5: Checkpoint C — merge the follow-up `chore: release` PR**

After CI passes on the follow-up PR, squash-merge it. release-plz publishes `paigasus-helikon-core@0.2.0` and `paigasus-helikon@0.1.1` to crates.io, creates the matching git tags, and creates GitHub releases.

Verify:

```bash
sleep 60  # crates.io index refresh
curl -s "https://crates.io/api/v1/crates/paigasus-helikon-core" | jq -r '.crate.max_version'
curl -s "https://crates.io/api/v1/crates/paigasus-helikon" | jq -r '.crate.max_version'
```

Expected: `0.2.0` and `0.1.1`.

- [ ] **Step 8.6: Checkpoint C continued — verify the SBOM workflow fired**

```bash
gh run list --workflow=sbom.yml --limit=5 --json databaseId,headBranch,event,status,conclusion,createdAt
```

Expected: a recent run triggered by `event: push` on the tag `paigasus-helikon-v0.1.1` (visible via the head SHA matching the tag's commit). Click through to confirm the SBOM artifact is attached to the GitHub release.

If no SBOM run appears, the tag-glob fix didn't take effect — re-check Step 5.1.

- [ ] **Step 8.7: Checkpoint D — confirm Linear auto-closes SMA-385**

```bash
PR_NUMBER=$(gh pr list --repo SMK1085/paigasus-helikon --state merged --search 'in:title SMA-385' --limit 1 --json number --jq '.[0].number')
gh pr view "$PR_NUMBER" --repo SMK1085/paigasus-helikon --json mergedAt,state
```

Expected: `state: "MERGED"` and `mergedAt` non-null. Linear auto-closes the SMA-385 issue when its PR merges (no manual status flip needed).

Forward-looking: Checkpoint D's stronger form ("future feat(core) commits trigger bumps") is verified by the next real feat(core) commit landing. Until that happens, the post-merge `chore: release` PR opening in Step 8.4 is the strongest signal that release-plz is healthy.

---

## Notes for executors

- **Commit order matters.** Tasks 1, 3, 4, 5, 6 produce five separate commits. Each commit is self-contained (passes `cargo build`/`cargo test` standalone); the pre-push hook enforces `convco check`, so commit messages must follow the Conventional Commits format in the example HEREDOCs.
- **Task 2 (pre-publish stubs) is manual.** A subagent cannot run `cargo publish` because crates.io publishes are irreversible. Sven must run Task 2 by hand before Task 3 lands. The plan halts at Task 2 if Sven hasn't completed it.
- **No `--no-verify` git pushes.** The pre-push hook enforces gates; if it complains, fix the underlying issue.
- **Don't amend.** If a commit is wrong, add a follow-up commit. Per CLAUDE.md's git safety section, amending in pre-commit-hook recovery loses work.
- **release-plz CI gates everything post-merge.** Checkpoints in Task 8 are observational, not actionable — if they fail, diagnose using the spec's §2 root cause, don't patch over.
