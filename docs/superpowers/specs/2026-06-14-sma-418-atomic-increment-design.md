# SMA-418 — Atomic compare-and-increment on `SessionState` (exact `max_uses` cap)

**Status:** design approved 2026-06-14
**Linear:** [SMA-418](https://linear.app/smaschek/issue/SMA-418/paigasus-helikon-core-atomic-compare-and-increment-on-sessionstate)
**Branch:** `feature/sma-418-paigasus-helikon-core-atomic-compare-and-increment-on`
**Related:** SMA-417 / PR #78 (CodeRabbit flagged the non-atomic `max_uses` counter as Major)

## Problem

`WebFetchTool::max_uses(N)` caps fetches per agent run. Today (`crates/paigasus-helikon-tools/src/web/fetch.rs:273-286`) it enforces the cap with three separate `SessionState` operations on each `invoke`:

```rust
let used = ctx.state().get(USES_KEY).and_then(|v| v.as_u64()).unwrap_or(0);
if used >= max as u64 { return Err(ToolError::Denied { .. }); }
ctx.state().set(USES_KEY, used + 1);
```

This is a read-then-compare-then-set with no atomicity across the three steps. Two invocations sharing a run (parallel sub-agents, parallel tool calls) can both read the same `used` and both proceed, overshooting the cap by up to the degree of concurrency. It is currently documented as a **best-effort** cap. This ticket makes it **exact**.

The only correct fix needs a new primitive on `paigasus-helikon-core::SessionState` — the sole run-scoped handle a tool has. `SessionState` is `Arc<Mutex<HashMap<String, serde_json::Value>>>`, so check+increment under a single lock hold is a small, contained addition.

## Decisions

- **Counting semantics — every attempt counts (unchanged).** The increment stays *up front*, before the network request and the SSRF vet, so a failed, non-2xx, or SSRF-blocked fetch still consumes a use. This keeps the cap a true abuse cap on attempts and makes the atomic primitive a clean drop-in. Rejected: "only successful fetches count," which would need a reserve-then-release pair (second decrement primitive, slot leak on crash between reserve and release).
- **Method name — `increment_u64_if_below`.** Self-documenting about both the stored type and the cap condition. Rejected: `try_increment_u64` and `increment_u64_capped` (cap condition implicit in the param).
- **Return type — `bool`.** `true` = incremented (admit), `false` = at/over cap (deny). Rejected: `Option<u64>` returning the new count — YAGNI; the only consumer needs admit/deny.
- **`u64`-specific, not a generic numeric CAS.** Scoped to the one consumer. Rejected: generic over `serde_json::Number` — out of scope.
- **Release model — pure-auto (no manual bumps).** Let release-plz drive core→tools→facade. Rejected: the ticket's same-PR manual core/facade bump, which targets the *ascend* deadlock and does not apply because `tools` is already released; applying it here would defeat the `dependencies_update` cascade. See §4.

## Design

### 1. Core API — `SessionState::increment_u64_if_below`

New method in `crates/paigasus-helikon-core/src/state.rs`, alongside `get`/`set`/`contains_key`/`keys`:

```rust
/// Atomically increment the `u64` at `key` if it is below `max`.
///
/// Reads the value at `key` (absent or non-`u64` ⇒ treated as `0`); if it is
/// `< max`, stores `value + 1` and returns `true`; otherwise leaves it and
/// returns `false`. The read-compare-write happens under a single lock hold,
/// so concurrent callers racing on the same key never collectively exceed `max`.
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

Edge cases, all handled naturally:

- `max = 0` → always `false` (zero fetches permitted).
- Absent or non-numeric value at `key` → treated as `0` (mirrors the existing `as_u64().unwrap_or(0)` leniency; the stored garbage is overwritten with `1` on the first admit).
- `current == u64::MAX` → cannot be `< max` for any `max <= u64::MAX`, so no overflow.

The critical section is a tiny synchronous read-compare-write; no `await` is held across the lock.

### 2. Rewire `WebFetchTool::invoke`

Replace the get/compare/set block (`fetch.rs:273-286`) with a single atomic call:

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

Same placement (up front, before the SSRF vet) preserves the every-attempt-counts semantics, now atomic. `USES_KEY` is unchanged.

Then update the `max_uses` doc comment (`fetch.rs:94-106`): drop the "best-effort abuse cap … the run-scoped counter is read-then-incremented, so simultaneous invocations … may overshoot the cap by up to the degree of concurrency" caveat and state the exact-cap guarantee. The replacement doc must make two things explicit:

- **Scope of "per run."** The cap is exact within one shared `SessionState` — which spans the agent run plus its handoff chain and its parallel sub-agents (`run_tools_concurrent` passes one `tool_ctx`; `subagent_child()` shares `state`). An `AgentAsTool` sub-run builds a **fresh** `RunContext` (separate `SessionState`), so it gets its **own** `max_uses` budget. Phrase it as "at most N per run, where a run is one shared `SessionState`; an `AgentAsTool` sub-run gets a fresh budget" so "exact N" isn't misread as global across all nested agents.
- **Every attempt counts.** Because the increment is up front (before the SSRF vet and network request), a failed, non-2xx, or SSRF-blocked fetch still consumes a use. This is a true *attempt* cap: a flaky network can exhaust the budget faster than the count of successful fetches, and an attacker probing internal IPs burns it (the latter is the intended behavior). State this contract plainly.

### 3. Test — concurrency proof

In the `state.rs` `#[cfg(test)]` module:

- **Concurrency test:** spawn `N` threads (e.g. 64), each holding a clone of one `SessionState`, each looping `increment_u64_if_below(key, MAX)` until it returns `false`, accumulating its local count of `true` results. Join all; assert the summed `true` count `== MAX` **and** the final stored value `== MAX`. Uses `std::thread` + `Arc`-backed clones — `SessionState` is synchronous, so no tokio runtime is needed.
- **Unit checks:** `max = 0` returns `false` and stores nothing; absent key is treated as `0` (first call returns `true`, stores `1`); a non-`u64` value at the key is treated as `0`.

(An optional integration-level assertion in `-tools` that `WebFetchTool` with `max_uses(N)` admits at most N under concurrent `invoke` may be added if it fits the existing `tests/web_fetch.rs` harness, but the core-level concurrency test is the canonical proof for the acceptance criteria.)

**Nature of the test (regression guard, not race observer).** Under a correct `Mutex` the `sum(true) == MAX` / `stored == MAX` invariant holds on every run, so the test cannot *observe* the old non-atomic race — it can't be made to flake against the fixed code. Its value is as a regression guard: a revert to the separate `get`/`set` sequence would very likely fail it under 64-thread contention. That is the intended role; keep it as the canonical proof.

### 4. Release coordination — pure-auto

**This deliberately diverges from the release approach prescribed in the ticket.** The ticket called for same-PR manual bumps of `core` + facade per the CLAUDE.md "same-PR-core-API rule." That rule exists to fix the **ascend deadlock**: when the *consuming* crate is **manually** bumped (`0.0.0 → 0.1.0` per the 4-step ascend recipe), release-plz's `release` step publishes it immediately — before release-plz has bumped `core` — so its `cargo publish --verify` builds against the stale registry `core` (path stripped at publish) and fails. That was SMA-321 (PR #45 failed; #46 fixed it reactively).

**`paigasus-helikon-tools` is already a released crate (`0.1.2`), not ascending.** So nothing is manually bumped, and release-plz drives everything:

- A single `feat(core): SMA-418 …` PR touches `core/` (new method + `state.rs` test) and `tools/` (the `fetch.rs` rewire + doc).
- On merge, release-plz's release PR bumps **core** (`feat` ⇒ **patch** on `0.x`: `0.5.1 → 0.5.2`), bumps **tools** (`0.1.2 → 0.1.3`), and — via `dependencies_update = true` — cascades a patch bump to the **facade** with refreshed dependency pins. (`release-plz.toml`'s own comment documents this cascade.)
- release-plz publishes in **dependency order: core → tools → facade**, so `tools`'s verify runs against the fresh `core` on the registry. No deadlock; the cascade stays intact so **no facade drift**.

No manual version edits, no manual `[workspace.dependencies]` pin edits, no hand-written CHANGELOG entries — release-plz owns all of it. **Reactive fallback only:** if a publish step unexpectedly deadlocks, fix it with a follow-up `chore(release): SMA-418 …` PR (the proven SMA-321/SMA-346 pattern). This is also why the squashed PR commit is a real `feat` (proper changelog entry for the new public method), not a `chore`.

(Versions above are the values current on `main` as of this design; release-plz reads whatever is current at release time.)

### 5. Branch & conventions

- Branch: `feature/sma-418-paigasus-helikon-core-atomic-compare-and-increment-on`.
- `cargo fmt --all` + `cargo clippy --workspace --all-features --all-targets -- -D warnings` before committing hand-edited Rust.
- PR title (gated by `pr-title.yml`): full Conventional Commits prefix + lowercase subject, e.g. `feat(core): SMA-418 add atomic increment_u64_if_below for exact max_uses cap`.

## Acceptance criteria (from the ticket)

- `SessionState` exposes an atomic increment-if-below; a concurrency unit test (N tasks racing on the same key) proves the counter never exceeds `max`.
- `WebFetchTool` with `max_uses(N)` admits **at most** N fetches per run even under concurrent invocation; the best-effort doc caveat is removed.
- core + facade bumped in the same PR; release publishes cleanly (core first). *(Intent satisfied via pure-auto per §4: release-plz bumps core/tools/facade and publishes core-first in dependency order. The literal "in the same PR" manual bump is superseded — it targets the ascend deadlock, which does not apply to an already-released `tools`.)*
- All CI gates green.

## Out of scope

- Generic numeric CAS / other typed atomics on `SessionState`.
- Changing what counts as a "use" (failed/blocked fetches continue to count).
- Any change to `WebSearchTool` or the SSRF guard.
