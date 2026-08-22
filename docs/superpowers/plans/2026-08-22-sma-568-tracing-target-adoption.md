# SMA-568 — `paigasus::*` Tracing Target Adoption Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every `tracing` call site under `crates/*/src` an explicit
`target: "paigasus::<component>::<subsystem>"`, so `paigasus::` means *all
Helikon events*, and add a workspace lint that keeps it that way.

**Architecture:** 41 untargeted call sites in `core` and the five runtime crates
gain explicit targets; `runtime-temporal`'s one existing target is renamed for
consistency. A new `scan_invocations` function in the internal
`paigasus-helikon-workspace-lints` member classifies every invocation's
`target:` argument, and a new integration test turns that into five assertions
(coverage, literal-ness, shape, component-matches-crate, no
`#[tracing::instrument]`). The mdBook chapter that documents the namespace is
rewritten from "two namespaces" to one, with a migration table.

**Tech Stack:** Rust 1.94 (MSRV), `tracing` 0.1, `tracing-subscriber` 0.3
(`env-filter`, dev-dependency only), mdBook, `cargo test --workspace
--all-features`.

**Spec:** `docs/superpowers/specs/2026-08-22-sma-568-tracing-target-adoption-design.md`

## Global Constraints

- **Component tier is stable API.** The eleven `paigasus::<component>` names in
  Task 8's `CRATE_COMPONENTS` map are a contract. Do not invent, abbreviate or
  re-spell one. The subsystem leaf is free.
- **No component name may be a prefix of another.** Enforced by
  `tests/workspace-lints/tests/tracing_target_docs.rs`.
- **No manual version bumps.** Do not edit any `version =` field, any
  `[workspace.dependencies]` pin, or any `CHANGELOG.md`. release-plz owns all
  of it. See spec §6.
- **No `!` in any commit or PR title, and no `BREAKING CHANGE:` footer
  anywhere.** Spec D4. Ordinary `feat(…)` / `test(…)` / `docs(…)` only.
- **`target:` not `target =`.** The `=` form silently records an ordinary field
  and leaves the event on its module path.
  `tests/workspace-lints/tests/tracing_target_syntax.rs` fails on `=`.
- **`target:` must come *before* `parent:`** in a `tracing` macro's argument
  list. Three sites in `core/src/agent.rs` pass `parent:` first.
- Commit scope allowlist (`.versionrc`): use only `core`, `runtime`, `lints`,
  `docs`, `spec`, `plan`. Commit subjects start lowercase after `SMA-568 `.
- Run `cargo fmt --all` before every commit. The `pre-push` hook runs
  `cargo fmt --all -- --check` and
  `cargo clippy --workspace --all-features --all-targets -- -D warnings`.
- Work in the worktree `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-568`
  on branch `feature/sma-568-decide-whether-core-and-the-runtime-crates-should-adopt`.
  Never check out another branch; the git tree is shared.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/paigasus-helikon-core/src/{agent,workflow,session,compacting_session,path_match}.rs` | 12 targets | 1 |
| `crates/paigasus-helikon-runtime-tokio/src/{lib,retry}.rs` | 2 targets | 2 |
| `crates/paigasus-helikon-runtime-temporal/src/{activities,activity_input,worker,runner}.rs` | 3 new + 1 renamed | 3 |
| `crates/paigasus-helikon-runtime-axum/src/{registry,error,handlers/runs}.rs` | 7 targets | 4 |
| `crates/paigasus-helikon-runtime-actix/src/{registry,error,handlers/runs}.rs` | 6 targets | 5 |
| `crates/paigasus-helikon-runtime-agentcore/src/{server,invoke,mcp,a2a/*,agui/mod}.rs` | 11 targets | 6 |
| `docs/book/src/concepts/observability-evaluation.md` | component table rows (Tasks 1–6), then the prose rewrite (Task 10) | 1–6, 10 |
| `tests/workspace-lints/src/lib.rs` | `TargetArg`, `Invocation`, `scan_invocations`; marker generalization; stale-comment fix | 7 |
| `tests/workspace-lints/tests/tracing_target_coverage.rs` | the five assertions + anti-vacuity | 8 |
| `tests/workspace-lints/tests/envfilter_semantics.rs` | pins `EnvFilter` raw-prefix behaviour | 9 |
| `tests/workspace-lints/Cargo.toml`, `Cargo.lock` | dev-deps for Task 9 | 9 |

**Why the conversions come before the guard.** The new coverage guard cannot
pass until all six crates are converted, so writing it first would mean six
consecutive red commits. Instead each conversion task gets its red-green cycle
from an **existing** test: adding a component row to the book makes
`tracing_target_docs.rs` fail ("documented but not in source"), and converting
the crate makes it pass. Every commit on the branch is green. Task 8 then adds
the ratchet that prevents regression.

**Known temporary inconsistency:** between Tasks 1 and 10 the book's *prose*
still says core and the runtimes emit on module paths, while the *table* and the
code say otherwise. This is confined to the branch and is resolved by Task 10.
Do not let a reviewer flag it as a defect before Task 10 runs.

---

## Task 1: `core` — 12 targets

**Files:**
- Modify: `crates/paigasus-helikon-core/src/agent.rs` (4 sites)
- Modify: `crates/paigasus-helikon-core/src/workflow.rs` (1 site)
- Modify: `crates/paigasus-helikon-core/src/session.rs` (3 sites)
- Modify: `crates/paigasus-helikon-core/src/compacting_session.rs` (2 sites)
- Modify: `crates/paigasus-helikon-core/src/path_match.rs` (2 sites)
- Modify: `docs/book/src/concepts/observability-evaluation.md` (1 table row)
- Test: `tests/workspace-lints/tests/tracing_target_docs.rs` (existing, unmodified)

**Interfaces:**
- Consumes: nothing.
- Produces: the component string `paigasus::core` and the subsystems `agent`,
  `workflow`, `session`, `compaction`, `permissions`. Task 8's
  `CRATE_COMPONENTS` map must contain `("paigasus-helikon-core", "core")`.

**The complete site list.** Line numbers are against `main@b6679108` and shift
as you edit within a file — **edit each file bottom-up (highest line first)**,
and confirm each site by its message text before editing.

| File | Line | Anchor (message text) | New target |
|---|---|---|---|
| `agent.rs` | 547 | `"tool.execute"` span, `parent: parent` | `paigasus::core::agent` |
| `agent.rs` | 734 | `"agent.run"` span | `paigasus::core::agent` |
| `agent.rs` | 858 | `"agent.turn"` span, `parent: &run_span` | `paigasus::core::agent` |
| `agent.rs` | 918 | `"gen_ai.chat"` span, `parent: chat_parent` | `paigasus::core::agent` |
| `workflow.rs` | 51 | `"agent.run"` span | `paigasus::core::workflow` |
| `session.rs` | 368 | `Compacted event with original_count = 0` | `paigasus::core::session` |
| `session.rs` | 373 | `Compacted event references more events` | `paigasus::core::session` |
| `session.rs` | 459 | `SessionRecorder: skipping Item::System` | `paigasus::core::session` |
| `compacting_session.rs` | 204 | `summarization failed; skipping compaction` | `paigasus::core::compaction` |
| `compacting_session.rs` | 210 | `model returned empty summary` | `paigasus::core::compaction` |
| `path_match.rs` | 144 | `invalid path-rule glob` | `paigasus::core::permissions` |
| `path_match.rs` | 150 | `path-rule globset build failed` | `paigasus::core::permissions` |

- [ ] **Step 1: Add the `core` row to the book's component table, making the existing guard fail**

In `docs/book/src/concepts/observability-evaluation.md`, inside the
`<!-- tracing-components:start … -->` / `<!-- tracing-components:end -->`
region, add one row directly **above** the `paigasus::openai` row:

```markdown
| `paigasus::core` | `paigasus-helikon-core` | `agent`, `workflow`, `session`, `compaction`, `permissions` | stable |
```

Change nothing else in the file in this task.

- [ ] **Step 2: Run the existing doc-drift guard to verify it fails**

```bash
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
```

Expected: **FAIL**, with a message naming `core` as documented but absent from
source.

- [ ] **Step 3: Convert the two `path_match.rs` sites**

Work bottom-up. Replace lines 150 and 144 (in that order):

```rust
        tracing::warn!(
            target: "paigasus::core::permissions",
            error = %e,
            "path-rule globset build failed; this rule will not match"
        );
```

```rust
                tracing::warn!(
                    target: "paigasus::core::permissions",
                    glob = %g,
                    error = %e,
                    "invalid path-rule glob; this rule will not match"
                );
```

Both were previously single-line invocations; expanding them to multi-line is
required because `cargo fmt` will not fit them on one line with the added
argument.

- [ ] **Step 4: Convert `compacting_session.rs`, `session.rs` and `workflow.rs`**

For each site in the table above, insert `target: "<new target>",` as the
**first** argument inside the macro's parentheses, on its own line. These five
sites have no `parent:` argument, so the insertion is unconditional. Example
for `session.rs:459`:

```rust
                    tracing::debug!(
                        target: "paigasus::core::session",
                        "SessionRecorder: skipping Item::System in input (no SessionEvent variant)"
                    );
```

Example for `workflow.rs:51` — a span macro, where `target:` goes before the
span name:

```rust
    let span = tracing::info_span!(
        target: "paigasus::core::workflow",
        "agent.run",
```

- [ ] **Step 5: Convert the four `agent.rs` sites, minding the `parent:` trap**

`tracing`'s macro arms require `target:` **before** `parent:`. Three of these
four sites pass `parent:` first, so the new argument goes ahead of it — a
mechanical "append to the argument list" edit will not compile.

`agent.rs:547`:

```rust
        let span = tracing::info_span!(
            target: "paigasus::core::agent",
            parent: parent,
            "tool.execute",
```

`agent.rs:858` and `agent.rs:918` follow the same shape, with
`parent: &run_span` and `parent: chat_parent` respectively. `agent.rs:734` has
no `parent:`, so `target:` is simply first.

- [ ] **Step 6: Format, then verify the guard now passes**

```bash
cargo fmt --all
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_syntax
```

Expected: both **PASS**. The syntax guard passing confirms you wrote `target:`
and not `target =` at all twelve sites.

- [ ] **Step 7: Verify `core` still builds and tests clean**

```bash
cargo test -p paigasus-helikon-core --all-features
```

Expected: PASS. A compile error mentioning `parent` means Step 5's ordering was
not applied.

- [ ] **Step 8: Commit**

```bash
git add crates/paigasus-helikon-core/src docs/book/src/concepts/observability-evaluation.md
git commit -m "feat(core): SMA-568 emit core events on paigasus::core targets"
```

---

## Task 2: `runtime-tokio` — 2 targets

**Files:**
- Modify: `crates/paigasus-helikon-runtime-tokio/src/lib.rs` (1 site)
- Modify: `crates/paigasus-helikon-runtime-tokio/src/retry.rs` (1 site)
- Modify: `docs/book/src/concepts/observability-evaluation.md` (1 table row)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: component `runtime_tokio`, subsystems `runner`, `retry`.

| File | Line | Anchor | New target |
|---|---|---|---|
| `lib.rs` | 108 | `session persistence failed during finalize` | `paigasus::runtime_tokio::runner` |
| `retry.rs` | 236 | `retrying model invoke after transient error` | `paigasus::runtime_tokio::retry` |

- [ ] **Step 1: Add the book row, making the guard fail**

Add below the `paigasus::litellm` row:

```markdown
| `paigasus::runtime_tokio` | `paigasus-helikon-runtime-tokio` | `runner`, `retry` | stable |
```

- [ ] **Step 2: Run the guard to verify it fails**

```bash
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
```

Expected: **FAIL**, naming `runtime_tokio`.

- [ ] **Step 3: Convert both sites**

`lib.rs:108`:

```rust
            tracing::warn!(
                target: "paigasus::runtime_tokio::runner",
                error = %e,
                "session persistence failed during finalize; run outcome unaffected"
            );
```

Keep whatever fields the existing invocation already passes — add only the
`target:` line as the first argument. `retry.rs:236` takes
`target: "paigasus::runtime_tokio::retry"` the same way.

- [ ] **Step 4: Format and verify**

```bash
cargo fmt --all
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_syntax
cargo test -p paigasus-helikon-runtime-tokio --all-features
```

Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-runtime-tokio/src docs/book/src/concepts/observability-evaluation.md
git commit -m "feat(runtime): SMA-568 emit runtime-tokio events on paigasus targets"
```

---

## Task 3: `runtime-temporal` — 3 new targets and 1 rename

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activities.rs` (rename)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activity_input.rs` (new)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/worker.rs` (new)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/runner.rs` (new)
- Modify: `docs/book/src/concepts/observability-evaluation.md` (rename one row)

**Interfaces:**
- Produces: component `runtime_temporal`, subsystems `activities`,
  `activity_input`, `worker`, `runner`. **The old component `temporal` ceases
  to exist** — Task 8's map must register `runtime_temporal`, never `temporal`.

| File | Line | Anchor | New target |
|---|---|---|---|
| `activities.rs` | 351 | `record_heartbeat failed; continuing` — **already targeted** `paigasus::temporal::activities` | `paigasus::runtime_temporal::activities` |
| `activity_input.rs` | 106 | `refused a pre-envelope activity input` | `paigasus::runtime_temporal::activity_input` |
| `worker.rs` | 489 | `assembling durable-agent Temporal worker` | `paigasus::runtime_temporal::worker` |
| `runner.rs` | 356 | `session persistence failed during finalize` | `paigasus::runtime_temporal::runner` |

This rename is permitted without a breaking marker: `temporal` is marked
*provisional* in the book today, and SMA-557's D1 states a provisional component
"may be renamed or removed in any release".

- [ ] **Step 1: Rename the book row and add the new subsystems**

Replace the existing `paigasus::temporal` row with:

```markdown
| `paigasus::runtime_temporal` | `paigasus-helikon-runtime-temporal` | `activities`, `activity_input`, `worker`, `runner` | stable |
```

Note the Status cell changes from `provisional` to `stable`. Leave the prose
paragraph about `paigasus::temporal` being provisional alone for now — Task 10
deletes it.

- [ ] **Step 2: Run the guard to verify it fails**

```bash
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
```

Expected: **FAIL** on two counts — `runtime_temporal` documented but absent from
source, and `temporal` present in source but no longer documented.

- [ ] **Step 3: Rename the existing target in `activities.rs`**

Change the existing `target: "paigasus::temporal::activities"` to
`target: "paigasus::runtime_temporal::activities"`. Do not touch anything else
in that invocation.

- [ ] **Step 4: Add targets to the three untargeted sites**

Insert `target: "<new target>",` as the first argument of each, per the table.
Example for `worker.rs:489`:

```rust
    tracing::debug!(
        target: "paigasus::runtime_temporal::worker",
        "assembling durable-agent Temporal worker"
    );
```

Preserve any existing fields on each invocation.

- [ ] **Step 5: Confirm no stale `paigasus::temporal` string survives**

```bash
grep -rn 'paigasus::temporal' crates/ docs/
```

Expected: **no output**. A hit under `docs/` means Step 1 missed the row; a hit
under `crates/` means Step 3 did not apply.

- [ ] **Step 6: Format and verify**

```bash
cargo fmt --all
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_syntax
cargo test -p paigasus-helikon-runtime-temporal --all-features
```

Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-runtime-temporal/src docs/book/src/concepts/observability-evaluation.md
git commit -m "feat(runtime): SMA-568 rename temporal component and target its remaining sites"
```

---

## Task 4: `runtime-axum` — 7 targets

**Files:**
- Modify: `crates/paigasus-helikon-runtime-axum/src/registry.rs` (4 sites)
- Modify: `crates/paigasus-helikon-runtime-axum/src/error.rs` (2 sites)
- Modify: `crates/paigasus-helikon-runtime-axum/src/handlers/runs.rs` (1 site)
- Modify: `docs/book/src/concepts/observability-evaluation.md` (1 table row)

**Interfaces:**
- Produces: component `runtime_axum`, subsystems `registry`, `error`, `runs`.

| File | Line | Anchor | New target |
|---|---|---|---|
| `registry.rs` | 80 | `run ended without a real terminal event` | `paigasus::runtime_axum::registry` |
| `registry.rs` | 187 | `rejecting run: in-flight limit reached` | `paigasus::runtime_axum::registry` |
| `registry.rs` | 308 | `reclaiming run that exceeded max_run_duration` | `paigasus::runtime_axum::registry` |
| `registry.rs` | 392 | `no Tokio runtime available; the run-registry sweeper` | `paigasus::runtime_axum::registry` |
| `error.rs` | 116 | `internal server error` | `paigasus::runtime_axum::error` |
| `error.rs` | 125 | `service unavailable` | `paigasus::runtime_axum::error` |
| `handlers/runs.rs` | 349 | `run failed to start` | `paigasus::runtime_axum::runs` |

- [ ] **Step 1: Add the book row, making the guard fail**

```markdown
| `paigasus::runtime_axum` | `paigasus-helikon-runtime-axum` | `registry`, `error`, `runs` | stable |
```

- [ ] **Step 2: Run the guard to verify it fails**

```bash
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
```

Expected: **FAIL**, naming `runtime_axum`.

- [ ] **Step 3: Convert all seven sites, editing each file bottom-up**

Insert `target: "<new target>",` as the first argument. None of these seven
passes `parent:`, so no ordering hazard applies. `registry.rs:392` wraps a
multi-line string continuation — keep the continuation exactly as it is and add
only the target line above it:

```rust
            tracing::warn!(
                target: "paigasus::runtime_axum::registry",
                "no Tokio runtime available; the run-registry sweeper was not spawned — runs \
                 exceeding max_run_duration will not be reclaimed until spawn_sweeper is called \
                 again from within an async context"
            );
```

- [ ] **Step 4: Format and verify**

```bash
cargo fmt --all
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_syntax
cargo test -p paigasus-helikon-runtime-axum --all-features
cargo build -p paigasus-helikon-runtime-axum --no-default-features
```

Expected: all PASS. The `--no-default-features` build matches the required
`build-no-default-features` CI job.

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-runtime-axum/src docs/book/src/concepts/observability-evaluation.md
git commit -m "feat(runtime): SMA-568 emit runtime-axum events on paigasus targets"
```

---

## Task 5: `runtime-actix` — 6 targets

**Files:**
- Modify: `crates/paigasus-helikon-runtime-actix/src/registry.rs` (3 sites)
- Modify: `crates/paigasus-helikon-runtime-actix/src/error.rs` (2 sites)
- Modify: `crates/paigasus-helikon-runtime-actix/src/handlers/runs.rs` (1 site)
- Modify: `docs/book/src/concepts/observability-evaluation.md` (1 table row)

**Interfaces:**
- Produces: component `runtime_actix`, subsystems `registry`, `error`, `runs`.

| File | Line | Anchor | New target |
|---|---|---|---|
| `registry.rs` | 79 | `run ended without a real terminal event` | `paigasus::runtime_actix::registry` |
| `registry.rs` | 187 | `rejecting run: in-flight limit reached` | `paigasus::runtime_actix::registry` |
| `registry.rs` | 308 | `reclaiming run that exceeded max_run_duration` | `paigasus::runtime_actix::registry` |
| `error.rs` | 112 | `internal server error` | `paigasus::runtime_actix::error` |
| `error.rs` | 121 | `service unavailable` | `paigasus::runtime_actix::error` |
| `handlers/runs.rs` | 399 | `run failed to start` | `paigasus::runtime_actix::runs` |

**Do not copy the axum targets.** These files are near-duplicates of
`runtime-axum`'s; a copy-paste that leaves `runtime_axum` in an actix file
compiles, passes the shape guard, and passes the docs guard. Only Task 8's D6
assertion catches it — and Task 8 has not run yet.

- [ ] **Step 1: Add the book row, making the guard fail**

```markdown
| `paigasus::runtime_actix` | `paigasus-helikon-runtime-actix` | `registry`, `error`, `runs` | stable |
```

- [ ] **Step 2: Run the guard to verify it fails**

```bash
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
```

Expected: **FAIL**, naming `runtime_actix`.

- [ ] **Step 3: Convert all six sites, editing each file bottom-up**

Insert `target: "<new target>",` as the first argument, per the table.

- [ ] **Step 4: Verify no axum target leaked into the actix crate**

```bash
grep -rn 'paigasus::runtime_axum' crates/paigasus-helikon-runtime-actix/
```

Expected: **no output**.

- [ ] **Step 5: Format and verify**

```bash
cargo fmt --all
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_syntax
cargo test -p paigasus-helikon-runtime-actix --all-features
cargo build -p paigasus-helikon-runtime-actix --no-default-features
```

Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-runtime-actix/src docs/book/src/concepts/observability-evaluation.md
git commit -m "feat(runtime): SMA-568 emit runtime-actix events on paigasus targets"
```

---

## Task 6: `runtime-agentcore` — 11 targets

**Files:**
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/server.rs` (1)
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/invoke.rs` (2)
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/mcp.rs` (1)
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/a2a/mod.rs` (1)
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/a2a/rpc.rs` (4)
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/a2a/store.rs` (1)
- Modify: `crates/paigasus-helikon-runtime-agentcore/src/agui/mod.rs` (1)
- Modify: `docs/book/src/concepts/observability-evaluation.md` (1 table row)

**Interfaces:**
- Produces: component `runtime_agentcore`, subsystems `server`, `invoke`,
  `mcp`, `a2a`, `agui`.

| File | Line | Anchor | New target |
|---|---|---|---|
| `server.rs` | 405 | `ready in {elapsed_ms}ms` | `paigasus::runtime_agentcore::server` |
| `invoke.rs` | 247 | `invocation client disconnected` | `paigasus::runtime_agentcore::invoke` |
| `invoke.rs` | 258 | `run task ended without reporting a result` | `paigasus::runtime_agentcore::invoke` |
| `mcp.rs` | 144 | `ready in {elapsed_ms}ms` | `paigasus::runtime_agentcore::mcp` |
| `a2a/mod.rs` | 59 | `ready in {elapsed_ms}ms` | `paigasus::runtime_agentcore::a2a` |
| `a2a/rpc.rs` | 203 | `a2a run task panicked` | `paigasus::runtime_agentcore::a2a` |
| `a2a/rpc.rs` | 610 | `a2a run failed` | `paigasus::runtime_agentcore::a2a` |
| `a2a/rpc.rs` | 623 | `could not store task artifacts` | `paigasus::runtime_agentcore::a2a` |
| `a2a/rpc.rs` | 657 | `could not append task event` | `paigasus::runtime_agentcore::a2a` |
| `a2a/store.rs` | 331 | `subscribe cursor pointed at evicted events` | `paigasus::runtime_agentcore::a2a` |
| `agui/mod.rs` | 68 | `ready in {elapsed_ms}ms` | `paigasus::runtime_agentcore::agui` |

The four `ready in {elapsed_ms}ms` sites are textually identical but live in
four different files and take **four different subsystems**. Match on the file,
not the message.

All six sites under `a2a/` share the subsystem `a2a` — the operator question is
"what is the A2A surface doing", which a `rpc`/`store` split would not answer
differently.

- [ ] **Step 1: Add the book row, making the guard fail**

```markdown
| `paigasus::runtime_agentcore` | `paigasus-helikon-runtime-agentcore` | `server`, `invoke`, `mcp`, `a2a`, `agui` | stable |
```

- [ ] **Step 2: Run the guard to verify it fails**

```bash
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
```

Expected: **FAIL**, naming `runtime_agentcore`.

- [ ] **Step 3: Convert all eleven sites, editing each file bottom-up**

Insert `target: "<new target>",` as the first argument, per the table.

- [ ] **Step 4: Format and verify**

```bash
cargo fmt --all
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_syntax
cargo test -p paigasus-helikon-runtime-agentcore --all-features
```

Expected: all PASS.

- [ ] **Step 5: Confirm the whole workspace is now targeted**

```bash
cargo test --workspace --all-features
```

Expected: PASS. This is the required CI gate; run it in full, not per-crate.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-runtime-agentcore/src docs/book/src/concepts/observability-evaluation.md
git commit -m "feat(runtime): SMA-568 emit runtime-agentcore events on paigasus targets"
```

---

## Task 7: `scan_invocations` — classify every invocation's target

**Files:**
- Modify: `tests/workspace-lints/src/lib.rs` (add types + function; generalize
  the allow-marker helper; fix the stale doc comment at lines 190–192)

**Interfaces:**
- Consumes: the existing private helpers `mask_trivia`, `ident_range_before`,
  `collect_macro_aliases`, `TRACING_MACROS`, and the public
  `MismatchedDelimiter`.
- Produces, for Tasks 8 and 9:
  ```rust
  pub enum TargetArg { Absent, NonLiteral, Literal(String) }
  pub struct Invocation { pub line: usize, pub macro_name: String, pub target: TargetArg }
  pub fn scan_invocations(src: &str) -> Result<Vec<Invocation>, MismatchedDelimiter>;
  pub const ALLOW_MARKER_COVERAGE: &str = "// allow(tracing-target-coverage)";
  pub fn allow_marker_lines(src: &str, marker: &str) -> std::collections::BTreeSet<usize>;
  ```

`Invocation::target` is `TargetArg::Absent` when the invocation passes no
`target:`, `NonLiteral` when it passes a non-string-literal value, and
`Literal(s)` with the literal's *content* (delimiters and any `r#` prefix
stripped) otherwise.

**Why one function with three states rather than two narrower ones:** Task 8
must distinguish "no target" from "computed target" from "literal target", and
a pair of functions returning only untargeted sites and only literal targets
cannot express the middle case. A computed target satisfies coverage but cannot
be shape-checked, which is exactly how an event could be routed out of the
namespace invisibly.

**Do not build this on `scan_targets`.** That function searches the masked
buffer for the bare needle `target:` and takes the next adjacent string literal
without checking it is inside a macro invocation at all. Step 1's third test
pins why that matters.

- [ ] **Step 1: Write the failing unit tests**

Add to the existing `#[cfg(test)] mod tests` in `tests/workspace-lints/src/lib.rs`:

```rust
#[test]
fn scan_invocations_classifies_a_literal_target() {
    let src = r#"fn f() { tracing::warn!(target: "paigasus::core::agent", "m"); }"#;
    let got = scan_invocations(src).expect("well-formed source");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].macro_name, "warn");
    assert_eq!(
        got[0].target,
        TargetArg::Literal("paigasus::core::agent".to_owned())
    );
}

#[test]
fn scan_invocations_reports_an_untargeted_site() {
    let src = r#"fn f() { tracing::warn!("m"); }"#;
    let got = scan_invocations(src).expect("well-formed source");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].target, TargetArg::Absent);
}

// Regression: `crates/paigasus-helikon-core/src/command_match.rs:436` contains
// `Redirect { target: "/etc/passwd".into() }` inside a `#[cfg(test)]` module.
// A needle-based scanner reads that as a tracing target and fails the shape
// assertion on a file that emits nothing at all.
#[test]
fn scan_invocations_ignores_a_struct_field_named_target() {
    let src = r#"fn f() { let r = Redirect { op: Op::Write, target: "/etc/passwd".into() }; }"#;
    assert_eq!(scan_invocations(src).expect("well-formed source"), vec![]);
}

#[test]
fn scan_invocations_classifies_a_non_literal_target() {
    let src = r#"fn f() { tracing::warn!(target: T_CORE, "m"); }"#;
    let got = scan_invocations(src).expect("well-formed source");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].target, TargetArg::NonLiteral);
}

#[test]
fn scan_invocations_sees_span_macros_and_bare_forms() {
    let src = r#"
        use tracing::warn;
        fn f() {
            let _ = tracing::info_span!(target: "paigasus::core::agent", parent: p, "agent.run");
            warn!("bare");
        }
    "#;
    let got = scan_invocations(src).expect("well-formed source");
    assert_eq!(got.len(), 2);
    assert_eq!(
        got[0].target,
        TargetArg::Literal("paigasus::core::agent".to_owned())
    );
    assert_eq!(got[1].target, TargetArg::Absent);
}

#[test]
fn scan_invocations_is_blind_to_comments_and_string_literals() {
    let src = r#"
        // tracing::warn!("commented out");
        fn f() { let s = "tracing::warn!(\"in a string\")"; }
    "#;
    assert_eq!(scan_invocations(src).expect("well-formed source"), vec![]);
}

#[test]
fn allow_marker_lines_finds_both_positions_and_ignores_string_literals() {
    let src = r#"
        // allow(tracing-target-coverage)
        fn a() { tracing::warn!("m"); }
        fn b() { tracing::warn!("m"); } // allow(tracing-target-coverage)
        fn c() { let s = "// allow(tracing-target-coverage)"; }
    "#;
    let lines = allow_marker_lines(src, ALLOW_MARKER_COVERAGE);
    assert_eq!(lines.len(), 2, "the marker inside a string literal must not count");
}
```

`TargetArg` and `Invocation` must derive `Debug, Clone, PartialEq, Eq` for
these assertions to compile.

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p paigasus-helikon-workspace-lints --lib
```

Expected: **FAIL** to compile — `scan_invocations`, `TargetArg`,
`allow_marker_lines` and `ALLOW_MARKER_COVERAGE` are not defined.

- [ ] **Step 3: Implement the types and the scanner**

Add to `tests/workspace-lints/src/lib.rs`. Reuse the existing invocation walker
in `try_scan` — the loop that finds `ident !` followed by `(`, `[` or `{`,
tracks delimiter depth to find the matching closer, and splits top-level
arguments. Extract that argument-walking into a shared private helper if
`try_scan` and `scan_invocations` would otherwise duplicate it.

Classification of the `target:` argument, once found among the top-level
arguments:
- absent → `TargetArg::Absent`
- present and the value's span coincides with an entry in
  `masked.string_literals` → `TargetArg::Literal(content)`
- present otherwise → `TargetArg::NonLiteral`

Every new `pub` item needs a `///` doc comment: the crate inherits
`[lints] workspace = true`, so `missing_docs = "warn"` applies and the required
`docs` CI job runs with `RUSTDOCFLAGS=-D warnings`.

Generalize the existing private `collect_allow_marker_lines` into the public
`allow_marker_lines(src, marker)`, keeping its comment-aware bookkeeping —
a plain `src.contains(…)` would also match the marker text inside a string
literal. Have the existing `try_scan` call the generalized form with
`ALLOW_MARKER` so its behaviour is unchanged.

- [ ] **Step 4: Run the tests to verify they pass, and that the existing guards still do**

```bash
cargo test -p paigasus-helikon-workspace-lints --all-targets
```

Expected: PASS, including the pre-existing `tracing_target_syntax` and
`tracing_target_docs` tests — the marker refactor must not change their
behaviour.

- [ ] **Step 5: Fix the stale doc comment**

`tests/workspace-lints/src/lib.rs:190-192` (in `scan_targets`'s doc comment)
states that no `target:` site outside a tracing macro exists in this workspace.
That is now false: `crates/paigasus-helikon-core/src/command_match.rs:436`,
`:445` and `:454` are exactly such sites. Rewrite those lines to say such sites
**do** exist, name `command_match.rs` as the example, and state that
`scan_targets` tolerates them only because a non-`paigasus::` literal yields
`None` from `component_of` — while `scan_invocations` is immune structurally.

- [ ] **Step 6: Format and commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-workspace-lints --all-targets -- -D warnings
git add tests/workspace-lints/src/lib.rs
git commit -m "test(lints): SMA-568 add scan_invocations target classifier"
```

---

## Task 8: The coverage, shape, component and instrument guard

**Files:**
- Create: `tests/workspace-lints/tests/tracing_target_coverage.rs`

**Interfaces:**
- Consumes: `scan_invocations`, `TargetArg`, `allow_marker_lines`,
  `ALLOW_MARKER_COVERAGE` from Task 7; the eleven component names from
  Tasks 1–6.
- Produces: nothing consumed downstream.

**The crate → component map.** All 21 workspace members, so no crate is
silently outside the rule. Eleven are live today; ten are **reserved** — they
emit nothing yet, so they must **not** appear in the book's component table
(the docs guard asserts book and source agree, and a row for an absent
component fails it).

| Crate directory | Component |
|---|---|
| `paigasus-helikon` | `facade` |
| `paigasus-helikon-cli` | `cli` |
| `paigasus-helikon-core` | `core` |
| `paigasus-helikon-evals` | `evals` |
| `paigasus-helikon-macros` | `macros` |
| `paigasus-helikon-mcp` | `mcp` |
| `paigasus-helikon-providers-openai` | `openai` |
| `paigasus-helikon-providers-anthropic` | `anthropic` |
| `paigasus-helikon-providers-bedrock` | `bedrock` |
| `paigasus-helikon-providers-gemini` | `gemini` |
| `paigasus-helikon-providers-litellm` | `litellm` |
| `paigasus-helikon-runtime-tokio` | `runtime_tokio` |
| `paigasus-helikon-runtime-axum` | `runtime_axum` |
| `paigasus-helikon-runtime-actix` | `runtime_actix` |
| `paigasus-helikon-runtime-agentcore` | `runtime_agentcore` |
| `paigasus-helikon-runtime-temporal` | `runtime_temporal` |
| `paigasus-helikon-sessions-sqlite` | `sessions_sqlite` |
| `paigasus-helikon-sessions-postgres` | `sessions_postgres` |
| `paigasus-helikon-sessions-redis` | `sessions_redis` |
| `paigasus-helikon-sessions-testkit` | `sessions_testkit` |
| `paigasus-helikon-tools` | `tools` |

Derivation rule, for a future crate: strip `paigasus-helikon-`, then strip a
leading `providers-`, then replace `-` with `_`. The providers' bare form is a
historical exception preserved because those names are the user-facing vendor
names.

- [ ] **Step 1: Write the guard test**

Create `tests/workspace-lints/tests/tracing_target_coverage.rs`. Copy the
`repo_root()` and `collect_rs()` helpers verbatim from
`tests/workspace-lints/tests/tracing_target_docs.rs` — they already handle the
symlink-recursion hazard and the `target`/`.git` skips.

Walk `<repo>/crates`, keeping only paths containing a `src` component, so
`crates/paigasus-helikon-providers-anthropic/tests/live.rs` (the one non-`src`
`tracing` user under `crates/`) stays out. Rooting at `<repo>/crates` rather
than the repo root also keeps `.claude/worktrees/` out, so a developer's
unrelated worktrees cannot change the verdict.

For each file, resolve its crate directory, look up the component, and over
`scan_invocations(src)?` — skipping invocations whose line is in
`allow_marker_lines(src, ALLOW_MARKER_COVERAGE)` or the line immediately after
one — assert:

1. `TargetArg::Absent` → fail, `path:line`, "carries no `target:`".
2. `TargetArg::NonLiteral` → fail, "`target:` is not a string literal; a
   computed target cannot be shape-checked".
3. `TargetArg::Literal(t)` not matching `^paigasus::[a-z0-9_]+::[a-z0-9_]+$` →
   fail, quoting `t`. Implement the match by hand (`strip_prefix("paigasus::")`
   then exactly one `::` split, both halves non-empty and all
   `c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'`) — the crate takes
   no dependencies, so there is no regex available.
4. `TargetArg::Literal(t)` whose component ≠ the crate's registered component →
   fail, naming both.
5. The file's masked source containing `#[tracing::instrument` or
   `#[instrument` → fail with "`#[tracing::instrument]` is not permitted under
   `crates/*/src`: the scanner keys on `ident !` and cannot see an attribute, so
   an instrumented function would silently emit on its module path (SMA-568 D7)".

Assertion 3 deliberately does **not** check the component against the book —
`tracing_target_docs.rs` already does, and duplicating it would mean two tests
reddening for one cause with two messages.

Also assert, as anti-vacuity:

- at least 100 `.rs` files were walked (the real population is 219, so this is a
  tripwire with headroom, not a coupling to workspace size);
- `scan_invocations` extracts `TargetArg::Literal("paigasus::openai::chat")`
  from `crates/paigasus-helikon-providers-openai/src/backend/chat.rs`, proving
  the scanner reads real source rather than returning a constant;
- `allow_marker_lines(src, ALLOW_MARKER_COVERAGE)` is empty for every walked
  file — no site uses the escape hatch today, so a first use is a fact a
  reviewer should see rather than a silent exemption.

Every `CRATE_COMPONENTS` key must also be asserted to exist as a directory, so
a renamed or removed crate fails loudly instead of falling through the lookup.

- [ ] **Step 2: Unit-test the shape predicate directly**

Write the shape check as a small named function in this test file so it can be
exercised without a filesystem walk, and add tests for it. The end-to-end
mutation checks in Step 4 cannot cover malformed shapes: introducing one into
`crates/` would also redden `tracing_target_docs.rs`, masking which guard
actually caught it.

```rust
#[test]
fn shape_predicate_accepts_only_three_lowercase_segments() {
    assert!(is_well_shaped("paigasus::core::agent"));
    assert!(is_well_shaped("paigasus::runtime_agentcore::a2a"));

    assert!(!is_well_shaped("paigasus::core"), "two segments");
    assert!(!is_well_shaped("paigasus::core::agent::extra"), "four segments");
    assert!(!is_well_shaped("paigasus::Core::agent"), "uppercase component");
    assert!(!is_well_shaped("paigasus::core::Agent"), "uppercase subsystem");
    assert!(!is_well_shaped("paigasus::::agent"), "empty component");
    assert!(!is_well_shaped("paigasus::core::"), "empty subsystem");
    assert!(!is_well_shaped("paigasus_helikon_core::agent"), "module path");
    assert!(!is_well_shaped("hyper::client::pool"), "foreign namespace");
}
```

Run:

```bash
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_coverage shape_predicate
```

Expected: PASS. `paigasus::core::agent::extra` must **fail** the predicate — a
naive "split on `::` and take the first two segments" implementation accepts it.

- [ ] **Step 3: Run the full guard and confirm it passes**

```bash
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_coverage
```

Expected: **PASS** — Tasks 1–6 already converted every site. If it fails, the
message names the exact `path:line` still outstanding; fix that site rather
than relaxing the guard.

- [ ] **Step 4: Mutation-check the guard against real source, both directions**

A guard that returns an empty finding list unconditionally passes on a clean
tree. Prove it does not:

```bash
# 3a. Break coverage.
sed -i '' 's|target: "paigasus::core::permissions",||' crates/paigasus-helikon-core/src/path_match.rs
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_coverage
# Expected: FAIL, naming path_match.rs
git checkout -- crates/paigasus-helikon-core/src/path_match.rs

# 3b. Break the component-matches-crate rule.
sed -i '' 's|paigasus::runtime_actix::registry|paigasus::runtime_axum::registry|' crates/paigasus-helikon-runtime-actix/src/registry.rs
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_coverage
# Expected: FAIL, naming runtime-actix and runtime_axum
git checkout -- crates/paigasus-helikon-runtime-actix/src/registry.rs
```

Confirm the tree is clean afterwards:

```bash
git status --short
```

Expected: no modified files under `crates/`.

- [ ] **Step 5: Run the full gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
```

Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add tests/workspace-lints/tests/tracing_target_coverage.rs
git commit -m "test(lints): SMA-568 require every tracing site to carry a paigasus target"
```

---

## Task 9: Pin the `EnvFilter` raw-prefix semantics

**Files:**
- Create: `tests/workspace-lints/tests/envfilter_semantics.rs`
- Modify: `tests/workspace-lints/Cargo.toml` (add `[dev-dependencies]`)
- Modify: `Cargo.lock`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: nothing consumed downstream.

The whole design rests on `EnvFilter` matching a target by **raw string prefix,
not by `::` segment**. SMA-557 verified that by hand at a REPL. This makes it a
regression test.

- [ ] **Step 1: Add the dev-dependencies**

In `tests/workspace-lints/Cargo.toml`, after the empty `[dependencies]`:

```toml
[dev-dependencies]
tracing            = { workspace = true }
tracing-subscriber = { workspace = true }
```

Both are already pinned in the root `[workspace.dependencies]`, with
`env-filter` among `tracing-subscriber`'s features. **Dev-dependencies only** —
nothing may enter `[dependencies]`, and no published crate gains a dependency.

- [ ] **Step 2: Write the failing test**

Create `tests/workspace-lints/tests/envfilter_semantics.rs`.

**Probe targets must be `const`s, never string literals.** `scan_targets`
requires a string literal adjacent to the `target:` needle, and
`tracing_target_docs.rs` walks `tests/` as well as `crates/`. Literal probes
would inject these components into that guard's source set permanently — so if
every real `runtime_tokio` site were later deleted, the docs guard would still
demand the book row, inverting the arm it exists for.

```rust
//! `EnvFilter` matches a target by raw string prefix, not by `::` segment.
//! SMA-568 D2 and D3 both depend on this; SMA-557 could only verify it by hand.

use std::sync::{Arc, Mutex};

use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::EnvFilter;

const T_CORE_AGENT: &str = "paigasus::core::agent";
const T_CORE_WORKFLOW: &str = "paigasus::core::workflow";
const T_OPENAI_CHAT: &str = "paigasus::openai::chat";
const T_AXUM_REGISTRY: &str = "paigasus::runtime_axum::registry";
const T_TOKIO_RETRY: &str = "paigasus::runtime_tokio::retry";
const T_MODULE_PATH: &str = "paigasus_helikon_core::session";
const T_FOREIGN: &str = "hyper::client";

/// Records the target of every event that reaches it.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<String>>>);

impl<S> Layer<S> for Capture
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        self.0
            .lock()
            .expect("capture mutex")
            .push(event.metadata().target().to_owned());
    }
}

/// Emit one DEBUG event per probe target under `$directive`, and evaluate to
/// the targets that survived filtering.
///
/// **This is a `macro_rules!`, not a function, and that is load-bearing.** A
/// `tracing` callsite caches its `Interest` globally, while `with_default`
/// installs a subscriber only for the current thread. A shared helper
/// *function* would give all four tests the same seven callsites, so whichever
/// test ran first would prime the interest cache for the rest — the classic
/// interest-caching flake, and a green-when-wrong one. A macro expands at each
/// invocation, so every test gets its own callsites and the tests stay
/// independent even running in parallel.
macro_rules! reaching {
    ($directive:expr) => {{
        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new($directive))
            .with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            tracing::event!(target: T_CORE_AGENT, tracing::Level::DEBUG, "p");
            tracing::event!(target: T_CORE_WORKFLOW, tracing::Level::DEBUG, "p");
            tracing::event!(target: T_OPENAI_CHAT, tracing::Level::DEBUG, "p");
            tracing::event!(target: T_AXUM_REGISTRY, tracing::Level::DEBUG, "p");
            tracing::event!(target: T_TOKIO_RETRY, tracing::Level::DEBUG, "p");
            tracing::event!(target: T_MODULE_PATH, tracing::Level::DEBUG, "p");
            tracing::event!(target: T_FOREIGN, tracing::Level::DEBUG, "p");
        });
        let out = capture.0.lock().expect("capture mutex").clone();
        out
    }};
}

#[test]
fn two_segment_component_selects_only_that_component() {
    let got = reaching!("paigasus::core=debug");
    assert!(got.contains(&T_CORE_AGENT.to_owned()));
    assert!(got.contains(&T_CORE_WORKFLOW.to_owned()));
    assert!(!got.contains(&T_OPENAI_CHAT.to_owned()));
    assert!(!got.contains(&T_AXUM_REGISTRY.to_owned()));
}

#[test]
fn runtime_group_selector_reaches_every_adapter() {
    let got = reaching!("paigasus::runtime=debug");
    assert!(got.contains(&T_AXUM_REGISTRY.to_owned()));
    assert!(got.contains(&T_TOKIO_RETRY.to_owned()));
    assert!(!got.contains(&T_CORE_AGENT.to_owned()));
    assert!(!got.contains(&T_OPENAI_CHAT.to_owned()));
}

#[test]
fn trailing_colons_select_the_namespace_and_exclude_module_paths() {
    let got = reaching!("paigasus::=debug");
    assert!(got.contains(&T_CORE_AGENT.to_owned()));
    assert!(got.contains(&T_OPENAI_CHAT.to_owned()));
    assert!(!got.contains(&T_MODULE_PATH.to_owned()));
    assert!(!got.contains(&T_FOREIGN.to_owned()));
}

// The load-bearing one: a bare `paigasus` is a raw prefix, so it ALSO matches
// `paigasus_helikon_core::session`. Everything in the book's "Filtering by
// target" section follows from this.
#[test]
fn bare_prefix_also_matches_module_paths() {
    let got = reaching!("paigasus=debug");
    assert!(got.contains(&T_CORE_AGENT.to_owned()));
    assert!(
        got.contains(&T_MODULE_PATH.to_owned()),
        "a bare `paigasus` directive must match `paigasus_helikon_*` too"
    );
    assert!(!got.contains(&T_FOREIGN.to_owned()));
}
```

- [ ] **Step 3: Run it**

```bash
cargo test -p paigasus-helikon-workspace-lints --test envfilter_semantics
```

Expected: PASS, all four tests. If `bare_prefix_also_matches_module_paths`
fails, stop — the spec's central premise is wrong and the book section must be
rewritten before anything else proceeds.

- [ ] **Step 4: Confirm the new test did not poison the docs guard**

```bash
cargo test -p paigasus-helikon-workspace-lints --test tracing_target_docs
```

Expected: PASS. A failure naming a component here means a probe was written as
a string literal instead of a `const`.

- [ ] **Step 5: Commit, including the lockfile**

```bash
cargo fmt --all
git add tests/workspace-lints/tests/envfilter_semantics.rs tests/workspace-lints/Cargo.toml Cargo.lock
git commit -m "test(lints): SMA-568 pin EnvFilter raw-prefix matching semantics"
```

`Cargo.lock` is committed in this repo and changes when the member gains
dev-dependencies. Do not leave it out.

---

## Task 10: Rewrite the book's "Filtering by target" section

**Files:**
- Modify: `docs/book/src/concepts/observability-evaluation.md`

**Interfaces:**
- Consumes: the eleven component names and their subsystems from Tasks 1–6.
- Produces: nothing consumed downstream.

Tasks 1–6 changed only the marked table region. Everything around it still
describes the old two-namespace world and is now wrong. This task fixes it.

- [ ] **Step 1: Rewrite the two-namespace opening**

The section currently opens "Helikon's targets come from two namespaces" and
lists `paigasus::<component>::<subsystem>` and `paigasus_helikon_*::…`. There
is now one. Say that every Helikon event and span carries a
`paigasus::<component>::<subsystem>` target, and that a workspace lint fails CI
if one stops doing so.

**Do not write "complete by construction".** The lint covers `tracing` macro
invocations under `crates/*/src`; it bans the one construct it cannot see
(`#[tracing::instrument]`), and it has an escape hatch a future contributor may
legitimately use. The honest claim — "every Helikon event carries one, and a
workspace lint fails CI if one stops doing so" — is true and checkable.

- [ ] **Step 2: Update the directive table**

Keep the existing rows and add the group selector. The `paigasus` row's
"Reaches" cell must stop claiming it catches `paigasus_helikon_core::session`
as a *live* example, since nothing emits there any more:

```markdown
| Directive | Reaches |
| --- | --- |
| `paigasus` | Raw prefix. Also matches any *non-Helikon* target beginning `paigasus` — a consuming application's own, say. See below. |
| `paigasus::` | The whole namespace. |
| `paigasus::runtime` | Includes every runtime adapter. A prefix of five components rather than a component, so it is not promised to match *only* them. |
| `paigasus::core` | One component. |
| `paigasus::core::agent` | One subsystem. Debugging only; the leaf may change in any release. |
```

- [ ] **Step 3: Replace "What is not in this namespace" with a migration section**

Delete the subsection listing the six crates that emit on module paths, and the
paragraph marking `paigasus::temporal` provisional and deferring the adoption
decision to a follow-up. Replace with a migration subsection stating plainly
that the old directives **stop matching** rather than becoming redundant:

```markdown
| Was | Now |
| --- | --- |
| `paigasus_helikon_core` | `paigasus::core` |
| `paigasus_helikon_runtime_tokio` | `paigasus::runtime_tokio` |
| `paigasus_helikon_runtime_temporal` | `paigasus::runtime_temporal` |
| `paigasus_helikon_runtime_axum` | `paigasus::runtime_axum` |
| `paigasus_helikon_runtime_actix` | `paigasus::runtime_actix` |
| `paigasus_helikon_runtime_agentcore` | `paigasus::runtime_agentcore` |
| `paigasus::temporal` | `paigasus::runtime_temporal` |
```

Do **not** name a version — release-plz decides it after merge, so any number
written now is a guess. Point at the crates' CHANGELOGs instead.

Add a second, equally prominent paragraph for the OpenTelemetry side, which is
easy to miss: `tracing-opentelemetry` sets `with_target: true` by default and
attaches a `target` **attribute** to every exported span and event. So a
Langfuse/Jaeger/Honeycomb saved search, sampling rule or dashboard filter keyed
on `target = "paigasus_helikon_core::agent"` goes silent and must be re-keyed
to `"paigasus::core::agent"`. Span *names* are unaffected.

- [ ] **Step 4: Keep the `workflow.rs` trap, updated**

The existing paragraph warning that `paigasus_helikon_core::agent` misses the
multi-agent run's top-level span (raised in `workflow.rs`) is still true at the
leaf tier. Keep it in substance, updated to `paigasus::core::agent` vs
`paigasus::core` — and note that the correct filter is now also the stable
two-segment form the stability rules already recommend.

- [ ] **Step 5: Record the reserved component names, outside the marked region**

State the derivation rule — strip `paigasus-helikon-`, then a leading
`providers-`, then `-` → `_` — and list the names reserved for crates that emit
nothing yet: `mcp`, `tools`, `evals`, `cli`, `sessions_sqlite`,
`sessions_postgres`, `sessions_redis`.

**These must not become table rows.** The docs guard asserts the book's
component set equals the source's, so a row for a component nothing emits fails
CI. Prose only, outside `<!-- tracing-components:start -->`.

- [ ] **Step 6: Fix the recipes and the `paigasus` vs `paigasus::` note**

Change `RUST_LOG='warn,paigasus_helikon_core=debug'` to
`RUST_LOG='warn,paigasus::core=debug'`, and add a runtime-group recipe such as
`RUST_LOG='warn,paigasus::runtime=debug'`.

In the `paigasus` vs `paigasus::` note, give both reasons to prefer the latter:
the equivalence holds only because a lint says so, not because the contract
does; and a consuming application with a `paigasus`-prefixed target of its own
is caught by the bare prefix and not by `paigasus::`.

- [ ] **Step 7: Build the book and run the full gate**

```bash
mdbook build docs/book
cargo test --workspace --all-features
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: all clean. `mdbook` runs with
`[output.linkcheck] warning-policy = "error"`, so a broken link fails the
required `book-build` job.

- [ ] **Step 8: Confirm no stale module-path guidance survives**

```bash
grep -n 'paigasus_helikon_core=debug\|paigasus::temporal\|What is not in this namespace' docs/book/src/concepts/observability-evaluation.md
```

Expected: matches only inside the migration table (where
`paigasus_helikon_core` appears as the "Was" column). No
`paigasus_helikon_core=debug` recipe, no `paigasus::temporal`, no
"What is not in this namespace" heading.

- [ ] **Step 9: Commit**

```bash
git add docs/book/src/concepts/observability-evaluation.md
git commit -m "docs(docs): SMA-568 document the unified paigasus tracing namespace"
```

---

## Final verification (before opening the PR)

- [ ] Every CI gate, in the exact form CI runs them:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo build -p paigasus-helikon-runtime-axum --no-default-features
cargo build -p paigasus-helikon-runtime-actix --no-default-features
mdbook build docs/book
```

- [ ] No manual version churn:

```bash
git diff main --stat -- '**/CHANGELOG.md' 'Cargo.toml' '**/Cargo.toml'
```

Expected: the only `Cargo.toml` change is `tests/workspace-lints/Cargo.toml`
gaining `[dev-dependencies]`. **No `version =` field changed anywhere, no
CHANGELOG touched.** If any did, remove the change — release-plz owns it, and a
manual bump defeats the facade cascade.

- [ ] No breaking marker reached the history:

```bash
git log main..HEAD --format='%s%n%b' | grep -n 'BREAKING CHANGE\|^[a-z]*(.*)!:'
```

Expected: no output.

- [ ] **Optional** manual spot check (spec §7.4). Requires a live
      `ANTHROPIC_API_KEY`, so it is not a gate:

```bash
RUST_LOG='paigasus::core=debug' cargo run -p paigasus-helikon-runtime-agentcore --example agent_http
```

Expected: the `agent.run` / `agent.turn` / `gen_ai.chat` / `tool.execute` spans
appear. Re-run with `RUST_LOG='paigasus_helikon_core=debug'` and confirm they do
**not**.

`agent_http` is the only example that both wires `EnvFilter` and drives a real
`LlmAgent`. Do not substitute either of the two examples earlier drafts named:
`langfuse_tracing.rs` installs no `EnvFilter` and exits early without live
Langfuse credentials, and `echo_http.rs` wires `EnvFilter` but its `EchoAgent`
implements `Agent` directly, never entering `LlmAgent`'s loop, so it raises none
of the four spans at any `RUST_LOG` setting.

Skipping this costs nothing: Task 8's assertion 4 proves core's four
`info_span!` sites carry `paigasus::core::agent`, and Task 9's
`two_segment_component_selects_only_that_component` proves
`paigasus::core=debug` selects that target. Their composition is the claim.
