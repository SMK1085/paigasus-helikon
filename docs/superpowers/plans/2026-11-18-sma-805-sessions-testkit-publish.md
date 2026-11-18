# SMA-805 PR 1: publish the sessions testkit — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `paigasus-helikon-sessions-testkit` publishable at `0.1.0`, so that the `release-plz` run of the merge commit publishes it, and document how an external backend runs the suite.

**Architecture:** No code logic changes. The change is packaging (`Cargo.toml`, `Cargo.lock`, `release-plz.toml`, a hand-written `CHANGELOG.md`), user docs for the testkit (README and crate-level rustdoc with one runnable doctest), and a sweep of every doc that calls the testkit "internal, never published".

**Tech Stack:** Rust 1.94 workspace, Cargo, release-plz, mdBook, markdownlint-cli2.

**Spec:** `docs/superpowers/specs/2026-11-18-sma-805-sessions-testkit-publish-design.md` (approved 2026-11-18). This plan covers "Design: PR 1" only. PR 2 (`ToolDeferred` test) is a separate follow-up.

## Global Constraints

- Work only in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-805-sessions-testkit-publish`, branch `feature/sma-805-publish-paigasus-helikon-sessions-testkit-to`. Do not run `git checkout`, `git switch`, `git reset`, `git rebase` or `git stash`. Do not push.
- Do not edit `paigasus-helikon-core` or the facade, and do not bump any version other than the testkit's.
- Testkit version: exactly `0.1.0`.
- Testkit description, exactly: `Session conformance suite for Paigasus Helikon backends, including your own.`
- The three backends keep their path-only testkit dev-dependency (`{ path = "../paigasus-helikon-sessions-testkit" }`) unchanged.
- Commit scopes must be on the `.versionrc` allowlist: use `chore(release)`, `docs(sessions)`, `docs(readme)`, `docs(claude)`, `docs(docs)` as given per task. End every commit message with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Stage files with explicit paths only. Never `git add -A` or `git add .` (`.env` and `.claude` are not ignored).
- Run every cargo command in the foreground and wait for it to finish. Do not end your turn while a command runs.
- Run `cargo fmt --all` before each commit that touches Rust or doc comments.

## Review Focus

- **A consumer outside the workspace** adds the published testkit and core from the registry: `run_all` must compile and pass against `MemorySession`. Pinned by Task 2, Step 5 (scratch crate outside the workspace).
- **A consumer that copies the README example** must get code that compiles: the README `#[tokio::test]` snippet uses the same imports as the doctest. Pinned by Task 2, Step 5 (the scratch crate uses the README snippet verbatim).
- **The tarball builds against the registry core**, not the path core: `cargo package` must succeed. Pinned by Task 1, Step 4.
- **The published manifest carries no path-only dependency and no removed dependency.** Pinned by Task 1, Step 5 (inspect the normalized `Cargo.toml` in the tarball).
- **No doc page still says the testkit is internal or unpublished.** Pinned by Task 3, Step 3 (grep sweep).

---

### Task 1: Packaging and release configuration

**Files:**
- Modify: `crates/paigasus-helikon-sessions-testkit/Cargo.toml`
- Modify: `Cargo.lock` (only the testkit entry; let cargo rewrite it)
- Modify: `release-plz.toml:29-60` (the testkit block and the comments around it)
- Create: `crates/paigasus-helikon-sessions-testkit/CHANGELOG.md`

**Interfaces:**
- Consumes: nothing.
- Produces: a packageable crate `paigasus-helikon-sessions-testkit` `0.1.0`. Its public API is unchanged: `run_all`, `run_append_read`, `run_watermark_exclusive`, `run_projection`, `run_concurrent_writers`, each `pub async fn run_x<F, Fut>(make: F) where F: Fn() -> Fut, Fut: Future<Output = Arc<dyn Session>>`.

- [ ] **Step 1: Confirm the current state fails the publish check**

Run: `cargo publish -p paigasus-helikon-sessions-testkit --dry-run 2>&1 | tail -3`
Expected: an error that the package cannot be published (`publish = false`).

- [ ] **Step 2: Edit `Cargo.toml`**

Set `version = "0.1.0"`. Delete the `publish = false` line. Replace the description with the exact text from Global Constraints. Delete the `async-trait` and `futures-util` lines from `[dependencies]`. Keep `paigasus-helikon-core`, `tokio` (features `macros`, `rt-multi-thread`), `jiff`, and `[lints] workspace = true`.

- [ ] **Step 3: Edit `release-plz.toml`**

Delete the testkit `[[package]]` block (`name = "paigasus-helikon-sessions-testkit"`, `publish = false`, `release = false`). Delete the "Stubs: pre-published at 0.0.0 ..." comment. Put one comment above the first remaining block that says: these three workspace members are internal test harnesses that never publish; `publish = false` in each `Cargo.toml` and `release = false` here must move together. Change "as the testkit above" in the `runtime-http-conformance` comment so it no longer names the testkit. Also update the comment at `release-plz.toml:21-24` if it says that the testkit is unpublished.

- [ ] **Step 4: Build and package**

Run: `cargo build -p paigasus-helikon-sessions-testkit && cargo test -p paigasus-helikon-sessions-testkit && cargo package -p paigasus-helikon-sessions-testkit --allow-dirty`
Expected: the build passes, `memory_session_passes_conformance` passes, and `cargo package` ends with `Packaged ...` and a successful `Verifying` step. `Cargo.lock` now shows `paigasus-helikon-sessions-testkit` `0.1.0` without `async-trait`/`futures-util` in its dependency list. Check with `git diff Cargo.lock` that only the testkit entry changed.

- [ ] **Step 5: Inspect the packaged manifest**

Run: `cat target/package/paigasus-helikon-sessions-testkit-0.1.0/Cargo.toml`
Expected: `version = "0.1.0"`; dependencies `paigasus-helikon-core` (with a `version` requirement and no `path`), `tokio`, `jiff`; no `async-trait`, no `futures-util`; `license = "Apache-2.0 OR MIT"`; `readme = "README.md"`; `repository` set.

- [ ] **Step 6: Write `CHANGELOG.md`**

Use the header of `crates/paigasus-helikon-sessions-sqlite/CHANGELOG.md:1-8` (Keep a Changelog text, then `## [Unreleased]`). Then add:

```markdown
## [0.1.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-sessions-testkit-v0.1.0) - 2026-11-18

### Added

- *(sessions)* SMA-805 first crates.io release of the shared `Session` conformance suite
  - `run_all` runs every conformance test against a factory that returns a fresh, empty `Arc<dyn Session>`
  - `run_append_read`, `run_watermark_exclusive`, `run_projection` and `run_concurrent_writers` are public, so a backend can run a subset
```

If the merge lands on a later date, the date in the heading may stay as is; release-plz does not rewrite it.

- [ ] **Step 7: Dry-run the publish**

Run: `cargo publish -p paigasus-helikon-sessions-testkit --dry-run --allow-dirty 2>&1 | tail -3`
Expected: ends with `aborting upload due to dry run` and no error.

- [ ] **Step 8: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-sessions-testkit/Cargo.toml crates/paigasus-helikon-sessions-testkit/CHANGELOG.md Cargo.lock release-plz.toml
git commit -m "chore(release): SMA-805 lift publish gates for the sessions testkit"
```

---

### Task 2: Testkit README and crate-level rustdoc

**Files:**
- Modify: `crates/paigasus-helikon-sessions-testkit/README.md` (full rewrite)
- Modify: `crates/paigasus-helikon-sessions-testkit/src/lib.rs:1-3` (crate-level `//!` docs only; no change to any function)
- Test: the doctest in `src/lib.rs`; a scratch crate under the session scratchpad (not committed)

**Interfaces:**
- Consumes: the `0.1.0` package from Task 1.
- Produces: user docs. No API change.

- [ ] **Step 1: Write the crate-level rustdoc with a runnable example**

Replace `src/lib.rs:1-3` with crate docs that cover, in this order: what the suite checks (the four tests, one line each); the factory contract (each call returns a fresh, empty session); the Tokio runtime requirement (`run_concurrent_writers` calls `tokio::spawn`, so `run_all` panics outside a runtime; recommend `multi_thread` with `worker_threads = 4`); the same-core-version rule with `cargo tree -d -i paigasus-helikon-core`; and the stability note (new sub-tests arrive in patch releases on 0.x; pin `=0.1.x` to control the upgrade). The example is a doctest with no `ignore`/`no_run`:

```rust
use std::sync::Arc;
use paigasus_helikon_core::{MemorySession, Session};
use paigasus_helikon_sessions_testkit::run_all;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    run_all(|| async { Arc::new(MemorySession::new()) as Arc<dyn Session> }).await;
}
```

- [ ] **Step 2: Run the doctest**

Run: `cargo test -p paigasus-helikon-sessions-testkit --doc`
Expected: `1 passed`. Then temporarily change the example to call `run_all` with a factory that returns the same session on every call (create one `Arc` outside the closure and clone it), run the command again, and confirm that the doctest FAILS (the second sub-test sees old events). Revert that change.

- [ ] **Step 3: Rewrite `README.md`**

Sections: a one-paragraph summary; "Install" with `cargo add --dev paigasus-helikon-sessions-testkit` and `cargo add --dev tokio --features macros,rt-multi-thread`; "Run the suite against your backend" with this test (text only, the same imports as Step 1):

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use paigasus_helikon_core::{MemorySession, Session};
use paigasus_helikon_sessions_testkit::run_all;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn my_session_passes_conformance() {
    let pid = std::process::id();
    let counter = AtomicU64::new(0);
    run_all(|| {
        let id = counter.fetch_add(1, Ordering::SeqCst);
        let _session_id = format!("conf-{pid}-{id}"); // use this id for a shared database
        async move { Arc::new(MemorySession::new()) as Arc<dyn Session> }
    })
    .await;
}
```

and a sentence that says: replace `MemorySession::new()` with your backend, opened under `_session_id`, so each call gets a fresh, empty session. Then "Run a subset" (the four public `run_*` functions); "Same core version" (the `cargo tree -d -i paigasus-helikon-core` check, and that no output means one version); "Stability" (the patch-release note and the `=0.1.x` pin); "License" (the same dual-license wording as `crates/paigasus-helikon-sessions-sqlite/README.md`). Do not mention `publish = false` or "internal".

- [ ] **Step 4: Lint the README**

Run: `npx markdownlint-cli2 crates/paigasus-helikon-sessions-testkit/README.md crates/paigasus-helikon-sessions-testkit/CHANGELOG.md`
Expected: `0 error(s)`.

- [ ] **Step 5: Check from outside the workspace**

Re-run `cargo package -p paigasus-helikon-sessions-testkit --allow-dirty` so the tarball has the new docs. Create a scratch crate at `$SCRATCH/testkit-consumer` (`SCRATCH=/private/tmp/claude-501/-Users-smaschek-dev-paigasus-paigasus-helikon/316ee59a-c614-461d-bef1-ecbd862e1b58/scratchpad`), outside the workspace, with an empty `[workspace]` table in its `Cargo.toml` so it does not join any parent workspace. Dev-dependencies: `paigasus-helikon-sessions-testkit = { path = "<worktree>/target/package/paigasus-helikon-sessions-testkit-0.1.0" }`, `paigasus-helikon-core = "0.5.21"` (registry), and `tokio` with `macros`, `rt-multi-thread`. Put the README test from Step 3, verbatim, in `tests/conformance.rs`.
Run: `cd "$SCRATCH/testkit-consumer" || exit 1; cargo test && cargo tree -d -i paigasus-helikon-core`
Expected: `my_session_passes_conformance ... ok`, and `cargo tree -d` prints nothing for core (one version). Do not commit the scratch crate.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-sessions-testkit/README.md crates/paigasus-helikon-sessions-testkit/src/lib.rs
git commit -m "docs(sessions): SMA-805 document how an external backend runs the testkit"
```

---

### Task 3: Doc sweep — the testkit is published

**Files:**
- Modify: `README.md:33` and the crate list at `README.md:35-50`
- Modify: `CLAUDE.md` (the "Every crate carries a real implementation ..." paragraph in "Workspace layout")
- Modify: `docs/book/src/introduction.md:19`
- Modify: `docs/book/src/reference/crates.md:17`, `:41`, `:43`, `:51`
- Modify: `docs/book/src/reference/api-docs.md:7-26` (list) and `:32` (publish status)
- Modify: `docs/book/src/getting-started/workspace-layout.md:73-75`
- Modify: `docs/book/src/concepts/sessions.md:336-348` ("Backend conformance")
- Modify: `docs/runbooks/release-ascend.md:15-19` (ascend recipe), the status snapshot at `:77`, and a new "If the first publish fails" subsection

**Interfaces:**
- Consumes: the README from Task 2 (link target `https://docs.rs/paigasus-helikon-sessions-testkit`).
- Produces: docs only.

- [ ] **Step 1: Edit the pages**

Exact content per page:
- Root `README.md:33`: "Twenty-one crates under `crates/`. All twenty-one are published to crates.io; one of them (the CLI) is binary-only, ...". Keep the rest of the sentence. Add a crate-list bullet: `**paigasus-helikon-sessions-testkit**` — the shared `Session` conformance suite; add it as a dev-dependency to test your own backend.
- `CLAUDE.md`: in "Workspace layout", change "Two standing exceptions: ..." so the testkit is no longer an exception. Keep the CLI exception. State that the testkit publishes as a normal library crate (SMA-805) and that the three backends keep a path-only dev-dependency on it.
- `introduction.md:19`: "Twenty-one crates are published ...".
- `crates.md`: in `:17` remove the testkit exception sentence. Row `:41` becomes `` | [`paigasus-helikon-sessions-testkit`](https://docs.rs/paigasus-helikon-sessions-testkit) | Shared `Session` conformance suite — run it against your own backend | published | ``. `:43`: every crate under `crates/` publishes; remove "the sole `publish = false` crate". `:51`: "All three are intentional non-published test harnesses, not stubs."
- `api-docs.md`: add a list bullet after `-sessions-redis`: `` [`paigasus-helikon-sessions-testkit`](https://docs.rs/paigasus-helikon-sessions-testkit) — shared `Session` conformance suite for any backend, including your own. See [Sessions](../concepts/sessions.md). `` `:32`: "All 21 crates under `crates/` publish to crates.io ..."; remove the "lone exception" sentence.
- `workspace-layout.md:73-75`: add the testkit to the published list above and replace the "rounds out the workspace's 21 without publishing" sentence: all 21 crates publish.
- `sessions.md`: in "Backend conformance", say "the `paigasus-helikon-sessions-testkit` crate" (no "internal"). Replace "and the forthcoming Postgres and Redis backends" with "`PostgresSession`, and `RedisSession`". Add a subsection "Run the suite against your own backend": two sentences and a link to the testkit README on docs.rs; name the factory contract and the `multi_thread` recommendation; no code copy.
- `release-ascend.md`: in the ascend recipe, add a note that the first publish of a never-claimed name needs a `CARGO_REGISTRY_TOKEN` with the `publish-new` scope and a matching crate scope, and that the crate publishes on the merge of its own PR, not via the release PR. Add a subsection "If a first publish fails" with the four steps from the spec ("If the first publish fails"). In the status snapshot (`:77`), replace the testkit sentence: it published in SMA-805 as `0.1.0`.

- [ ] **Step 2: Build and lint the docs**

Run: `mdbook build docs/book && npx markdownlint-cli2 && bash scripts/check-markdownlint-config.sh`
Expected: `mdbook` finishes with no linkcheck error (the new docs.rs link is external; if linkcheck rejects it because the page does not exist yet, report that and stop, do not remove the link); markdownlint prints `0 error(s)`.

- [ ] **Step 3: Sweep for stale wording**

Run: `git grep -n -i -E "testkit.{0,80}(internal|publish = false|never published|unpublished)|(internal|unpublished).{0,80}testkit|Twenty (are|crates are) published|20 non-internal" -- ':!docs/superpowers' ':!**/CHANGELOG.md'`
Expected: no match, except `tests/workspace-lints/tests/tracing_target_coverage.rs` (a tracing-name roster, not a publish claim) and `observability-evaluation.md` (a tracing-name list). Fix any other match.

- [ ] **Step 4: Commit**

```bash
git add README.md CLAUDE.md docs/book/src/introduction.md docs/book/src/reference/crates.md docs/book/src/reference/api-docs.md docs/book/src/getting-started/workspace-layout.md docs/book/src/concepts/sessions.md docs/runbooks/release-ascend.md
git commit -m "docs(docs): SMA-805 describe the sessions testkit as published"
```

---

### Task 4: Full CI gate run

**Files:** none (verification only; fix and commit only if a gate fails).

- [ ] **Step 1: Run every gate from CLAUDE.md**

Run each, in order:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
npx markdownlint-cli2
bash scripts/check-markdownlint-config.sh
bash scripts/check-cargo-profile-env-sync.sh
bash scripts/check-cargo-profile-env-sync-selftest.sh
bash scripts/check-release-gaps-selftest.sh
mdbook build docs/book
```

Expected: each exits 0. Known local noise: on macOS, `cargo test --workspace --all-features` in the main checkout path can show ~48 bedrock `NATIVE_ROOTS` failures; that tracks the checkout path, not the code. If it appears, run the bedrock tests from a worktree under the scratchpad to confirm, and report it. Report any other failure with its output; do not hide it.

- [ ] **Step 2: Confirm the diff matches the plan**

Run: `git diff --stat origin/main...HEAD`
Expected: only the files named in Tasks 1-3, plus the spec and this plan. No backend `Cargo.toml`, no core file, no facade file.
