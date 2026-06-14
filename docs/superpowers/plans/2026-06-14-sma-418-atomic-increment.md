# SMA-418 — Atomic `increment_u64_if_below` on `SessionState` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `WebFetchTool::max_uses(N)` an exact per-run cap under concurrency by adding an atomic check-and-increment primitive to `paigasus-helikon-core::SessionState` and rewiring the tool to use it.

**Architecture:** `SessionState` is `Arc<Mutex<HashMap<String, serde_json::Value>>>`. Add one method, `increment_u64_if_below(key, max) -> bool`, that does read-compare-write under a single lock hold (atomic). Replace `WebFetchTool::invoke`'s three-step `get`/compare/`set` sequence with one call to it. No semantics change beyond closing the race: the use is still consumed up front (every attempt counts). Release is **pure-auto** — no manual version bumps; release-plz bumps core→tools→facade in dependency order.

**Tech Stack:** Rust, `std::sync::Mutex`, `serde_json`, `std::thread` (test only). Workspace crates `paigasus-helikon-core` and `paigasus-helikon-tools`.

**Design doc:** `docs/superpowers/specs/2026-06-14-sma-418-atomic-increment-design.md`

---

## File Structure

- **Modify** `crates/paigasus-helikon-core/src/state.rs` — add the `increment_u64_if_below` method to `impl SessionState` (alongside `get`/`set`/`contains_key`/`keys`), plus two tests in the existing `#[cfg(test)] mod tests`.
- **Modify** `crates/paigasus-helikon-tools/src/web/fetch.rs` — replace the cap block in `invoke` (currently lines ~273–286) and rewrite the `max_uses` builder doc comment (currently lines ~94–106). Constant `USES_KEY` (line 18) is unchanged.

No new files. The existing `crates/paigasus-helikon-tools/tests/web_fetch.rs::max_uses_caps_fetches_per_run` is the behavior regression guard for the rewire and must stay green.

---

## Task 1: Atomic primitive on `SessionState` (core)

**Files:**
- Modify: `crates/paigasus-helikon-core/src/state.rs` (impl block ~lines 21–61; tests module ~lines 105–135)
- Test: same file, `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing tests**

Add these two tests to the `#[cfg(test)] mod tests` block in `crates/paigasus-helikon-core/src/state.rs`, after the existing `keys_lists_all` test:

```rust
    #[test]
    fn increment_if_below_edge_cases() {
        let s = SessionState::new();

        // Absent key ⇒ treated as 0; first admits store 1, then 2.
        assert!(s.increment_u64_if_below("k", 2));
        assert_eq!(s.get("k").and_then(|v| v.as_u64()), Some(1));
        assert!(s.increment_u64_if_below("k", 2));
        assert_eq!(s.get("k").and_then(|v| v.as_u64()), Some(2));

        // At the cap ⇒ false, value unchanged.
        assert!(!s.increment_u64_if_below("k", 2));
        assert_eq!(s.get("k").and_then(|v| v.as_u64()), Some(2));

        // max = 0 ⇒ always false, nothing stored.
        assert!(!s.increment_u64_if_below("zero", 0));
        assert!(s.get("zero").is_none());

        // Non-u64 value ⇒ treated as 0, overwritten with 1.
        s.set("garbage", "not a number");
        assert!(s.increment_u64_if_below("garbage", 1));
        assert_eq!(s.get("garbage").and_then(|v| v.as_u64()), Some(1));
    }

    #[test]
    fn increment_if_below_is_atomic_under_contention() {
        use std::thread;

        const MAX: u64 = 1000;
        const THREADS: usize = 64;
        let s = SessionState::new();

        // Every thread races to admit on the same key until the cap is hit,
        // tallying how many admits (true) it saw.
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let s = s.clone();
                thread::spawn(move || {
                    let mut local = 0u64;
                    while s.increment_u64_if_below("uses", MAX) {
                        local += 1;
                    }
                    local
                })
            })
            .collect();

        let total: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();

        // Exactly MAX admits across all threads, and the stored counter lands
        // on MAX — never above it. A non-atomic get/set would overshoot here.
        assert_eq!(total, MAX, "exactly MAX admits across all threads");
        assert_eq!(s.get("uses").and_then(|v| v.as_u64()), Some(MAX));
    }
```

- [ ] **Step 2: Run the tests to verify they fail (compile error)**

Run: `cargo test -p paigasus-helikon-core state::tests::increment 2>&1 | tail -20`
Expected: FAIL — compile error, `no method named increment_u64_if_below found for struct SessionState`.

- [ ] **Step 3: Implement the method**

Add this method inside `impl SessionState` in `crates/paigasus-helikon-core/src/state.rs`, after `set` (keep it before `contains_key` or after `keys` — anywhere in the impl block):

```rust
    /// Atomically increment the `u64` at `key` if it is below `max`.
    ///
    /// Reads the value at `key` (absent or non-`u64` ⇒ treated as `0`); if it
    /// is `< max`, stores `value + 1` and returns `true`; otherwise leaves it
    /// untouched and returns `false`. The read-compare-write happens under a
    /// single lock hold, so concurrent callers racing on the same key never
    /// collectively exceed `max`.
    pub fn increment_u64_if_below(&self, key: &str, max: u64) -> bool {
        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let current = guard.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
        if current < max {
            guard.insert(key.to_owned(), serde_json::Value::from(current + 1));
            true
        } else {
            false
        }
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-core state::tests 2>&1 | tail -20`
Expected: PASS — all `state::tests` pass, including `increment_if_below_edge_cases` and `increment_if_below_is_atomic_under_contention`.

- [ ] **Step 5: Format and lint the core crate**

Run: `cargo fmt --all && cargo clippy -p paigasus-helikon-core --all-features --all-targets -- -D warnings`
Expected: no diff from fmt, no clippy warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-core/src/state.rs
git commit -m "feat(core): SMA-418 add atomic increment_u64_if_below on SessionState"
```

---

## Task 2: Rewire `WebFetchTool` and update the `max_uses` doc (tools)

**Files:**
- Modify: `crates/paigasus-helikon-tools/src/web/fetch.rs` (cap block ~lines 273–286; `max_uses` doc ~lines 94–106)
- Regression guard (do not edit): `crates/paigasus-helikon-tools/tests/web_fetch.rs::max_uses_caps_fetches_per_run`

This is a behavior-preserving rewire (the use is still consumed up front), guarded by the existing `max_uses_caps_fetches_per_run` test — so there is no new failing test to write here; the discipline is "the existing test stays green and the new doc builds clean."

- [ ] **Step 1: Replace the cap block in `invoke`**

In `crates/paigasus-helikon-tools/src/web/fetch.rs`, replace this exact block:

```rust
        // Per-run use cap (run-scoped via the shared SessionState).
        if let Some(max) = self.max_uses {
            let used = ctx
                .state()
                .get(USES_KEY)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if used >= max as u64 {
                return Err(ToolError::Denied {
                    reason: format!("WebFetch use limit reached ({max} fetches per run)"),
                });
            }
            ctx.state().set(USES_KEY, used + 1);
        }
```

with:

```rust
        // Per-run use cap (run-scoped, atomic via the shared SessionState).
        if let Some(max) = self.max_uses {
            if !ctx.state().increment_u64_if_below(USES_KEY, max as u64) {
                return Err(ToolError::Denied {
                    reason: format!("WebFetch use limit reached ({max} fetches per run)"),
                });
            }
        }
```

- [ ] **Step 2: Rewrite the `max_uses` builder doc comment**

In the same file, replace this exact doc comment (the lines immediately above `pub fn max_uses(mut self, n: usize) -> Self {`):

```rust
    /// Cap the number of fetches this tool may perform within a single agent
    /// run (tracked run-scoped via [`ToolContext`]'s state). The `(n+1)`th fetch
    /// in a run is refused with [`ToolError::Denied`]. Default: unlimited.
    ///
    /// This is a **best-effort abuse cap**, not a hard concurrency limit: the
    /// run-scoped counter is read-then-incremented, so simultaneous invocations
    /// (e.g. parallel sub-agents sharing the run) may overshoot the cap by up to
    /// the degree of concurrency. The security boundary is the SSRF guard, not
    /// this counter.
```

with:

```rust
    /// Cap the number of fetches this tool may perform within a single agent
    /// run. The `(n+1)`th fetch in a run is refused with [`ToolError::Denied`].
    /// Default: unlimited.
    ///
    /// The cap is **exact**: the run-scoped counter is bumped with an atomic
    /// check-and-increment on the shared `SessionState`, so even simultaneous
    /// invocations (parallel sub-agents or parallel tool calls sharing the run)
    /// admit **at most** `n` fetches in total. "Per run" means one shared
    /// `SessionState` — the agent run plus its handoff chain and parallel
    /// sub-agents; an `AgentAsTool` sub-run uses a fresh `SessionState` and so
    /// gets its own independent budget.
    ///
    /// Every *attempt* counts: the use is consumed up front, before the SSRF
    /// vet and the network request, so a failed, non-2xx, or SSRF-blocked fetch
    /// still spends one of the `n`. The security boundary is the SSRF guard, not
    /// this counter.
```

(Plain-text reference to `SessionState`/the new method — no intra-doc link — to avoid pulling an import in solely for a doc link, which would trip `unused_imports` under `-D warnings`. `[`ToolError::Denied`]` stays a link; `ToolError` is already imported.)

- [ ] **Step 3: Run the tools web tests (regression guard stays green)**

Run: `cargo test -p paigasus-helikon-tools --features web --test web_fetch 2>&1 | tail -25`
Expected: PASS — all tests, including `max_uses_caps_fetches_per_run`, pass.

- [ ] **Step 4: Format and lint the workspace**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-features --all-targets -- -D warnings`
Expected: clean — no formatting diff, no clippy warnings.

- [ ] **Step 5: Build docs with warnings-as-errors (catches the doc rewrite)**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps 2>&1 | tail -15`
Expected: docs build with no warnings (no broken intra-doc links).

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-tools/src/web/fetch.rs
git commit -m "fix(tools): SMA-418 use atomic SessionState cap so max_uses is exact"
```

---

## Task 3: Full local CI gates, push, and open the PR (pure-auto release)

**Files:** none modified. This task verifies and ships.

No manual version bumps and no CHANGELOG edits — `tools` is an already-released crate (not ascending), so release-plz bumps **core → tools → facade** itself in dependency order (core publishes before `tools`'s `cargo publish --verify`, cascade intact, no facade drift). See the design doc §4. Reactive fallback only: if a publish step deadlocks, fix it with a follow-up `chore(release): SMA-418 …` PR.

- [ ] **Step 1: Run the full CI gate suite locally**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```
Expected: every command exits 0. (`convco`/doc-coverage gates run in CI; the four above are the fast local mirror.)

- [ ] **Step 2: Confirm no stray version/manifest changes**

Run: `git status --short && git diff --stat`
Expected: only `crates/paigasus-helikon-core/src/state.rs` and `crates/paigasus-helikon-tools/src/web/fetch.rs` touched across the two task commits; **no** `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, or `release-plz.toml` changes.

- [ ] **Step 3: Push the feature branch**

```bash
git push -u origin feature/sma-418-paigasus-helikon-core-atomic-compare-and-increment-on
```
Expected: branch pushed (matches the `feature/**` ruleset). If signing fails with "failed to fill whole buffer", the 1Password vault is locked — ask the user to unlock, then retry; do not bypass signing.

- [ ] **Step 4: Open the PR with a gate-passing title**

```bash
gh pr create \
  --base main \
  --title "feat(core): SMA-418 add atomic increment_u64_if_below for exact max_uses cap" \
  --body "$(cat <<'EOF'
Closes SMA-418.

Adds `SessionState::increment_u64_if_below(key, max) -> bool` — an atomic
check-and-increment under the existing single `Mutex` — and rewires
`WebFetchTool::invoke` to use it, replacing the non-atomic get/compare/set
that let concurrent invocations overshoot `max_uses` (CodeRabbit "Major" on
PR #78). Semantics are unchanged: the use is consumed up front, so every
attempt (including failed/SSRF-blocked) still counts. The "best-effort under
concurrency" caveat is removed from the `max_uses` doc.

A 64-thread concurrency test in `state.rs` proves exactly `max` admits and a
final counter of `max`. The existing `max_uses_caps_fetches_per_run` tools
test guards the rewire.

Release: pure-auto — no manual bumps; release-plz drives core → tools →
facade in dependency order.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```
Expected: PR created; title passes `pr-title.yml` (full `type(scope):` prefix, lowercase subject after `SMA-418 `).

- [ ] **Step 5: Verify CI and CodeRabbit**

After CI settles, confirm the required contexts are green: `fmt`, `clippy`, `test (ubuntu-latest, stable)`, `docs`, `doc-coverage`, `commits`, `pr-title`, `audit`, `deny`. Cross-check each required check has actually **reported** (not just that visible ones passed). For CodeRabbit, query `gh api repos/<owner>/<repo>/pulls/<n>/reviews` directly rather than trusting the status row.

---

## Notes on scope (from the spec)

- **Out of scope:** generic numeric CAS / other typed atomics on `SessionState`; changing what counts as a "use"; any `WebSearchTool` or SSRF-guard change.
- The optional `-tools`-level concurrency assertion mentioned in the spec is intentionally **omitted**: the core 64-thread test is the canonical proof of atomicity, and the single-threaded `max_uses_caps_fetches_per_run` test already guards the tool wiring. Add the heavier `wiremock`-backed concurrent-`invoke` test later only if a regression slips past these two.
