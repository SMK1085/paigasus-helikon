# SMA-447 — forkd microVM: GC / reconciliation of orphaned sandboxes — design

**Ticket:** [SMA-447](https://linear.app/smaschek/issue/SMA-447/paigasus-helikon-tools-forkd-microvm-gcreconciliation-of-orphaned)
**Branch:** `feature/sma-447-paigasus-helikon-tools-forkd-microvm-gcreconciliation-of`
**Follows:** SMA-416 (skeleton that first named the gap), SMA-437 (egress + live validation; consciously re-deferred this in its spec §10).
**Crate:** `paigasus-helikon-tools` (already published, `0.2.7`), file `crates/paigasus-helikon-tools/src/exec/forkd.rs`, feature `microvm`.

> **Revised after adversarial spec-challenge** (see §13 for the finding-by-finding
> disposition). The biggest change: the `GET /v1/sandboxes` wire contract is treated
> as an **explicitly-unverified assumption** validated on the GCP harness, and the
> deserialization is hardened so a field mismatch surfaces loudly (`skipped_unageable`)
> instead of silently reaping nothing.

---

## 1. Problem

`ForkdBackend::run()` drives fork → exec → destroy. In `fork()` the controller
**commits a real microVM** and returns its id in the HTTP response body. If the
client never decodes that body — a JSON decode error, or the `tokio::time::timeout`
around `fork()` firing **after** the controller already created the VM — the backend
holds no sandbox id and can therefore never issue the `DELETE`.

The microVM then **leaks**: pinned guest RAM + a Firecracker process + a cgroup on
the forkd host, with no handle to reap it. forkd's only enforced quota is
`memory.max` (SMA-416 spike §3), so on a live KVM host this is an unbounded resource
leak under repeated failures.

The window is narrow — it requires a decode/timeout **after** a committed fork —
which is exactly why SMA-437 deferred it (spec §10) rather than expanding that PR.

## 2. Goal & non-goals

**Goal:** a minimal, operator-triggered reconciliation pass that lists the
controller's sandboxes, identifies orphans belonging to this backend's snapshot tag,
and reaps them — plus fixing the now-stale orphan-window comment.

**Non-goals (YAGNI — see §10):** a background sweeper task, in-flight id tracking, an
immediate decode-path reap, `forkd cleanup` work-dir GC, cross-process coordination
beyond the age heuristic, and LIST pagination.

## 3. Design decisions (resolved in brainstorming)

| # | Decision | Choice | Why |
|---|----------|--------|-----|
| D1 | Trigger model | **Manual `reconcile()` only** | A library should not spawn/own a background task: no async-`Drop`, no leaked task, no runtime-handle capture; fully wiremock-testable. The operator owns cadence (cron / on-shutdown). Matches the "portable HTTP client" premise (SMA-416 §3). |
| D2 | Orphan test | **Age-only threshold** | Stateless and multi-process-safe. A healthy `run()` destroys its VM right after exec, so any tag-matching VM older than `reap_age` is an orphan or a hung-past-timeout run — both leaks. An in-flight id set cannot help (the orphan's id was *never learned*) and is wrong across processes sharing a tag. |
| D3 | Decode-path immediate reap | **Out of scope** | Under age-only we have no id, so an inline reap could only target *recent* tag-matching VMs — which would kill concurrent freshly-forked runs. Defer to the next age-based sweep. |
| D4 | `forkd cleanup` subcommand | **Out of scope** | It is a host-local CLI, not REST; shelling out breaks the portable-HTTP-client model and assumes co-location + the binary on `PATH`. The REST `DELETE` already triggers forkd's own work-dir teardown; periodic `cleanup` is an operator cron concern. |

## 4. Public API surface (additions)

Three additions, all forkd-specific. `reconcile()` is an **inherent method on
`ForkdBackend`**, *not* on the shared `ExecutionBackend` trait — host / os_sandbox /
seatbelt backends have no controller to reconcile against, so widening the trait
would be wrong.

```rust
impl ForkdBackendBuilder {
    /// Minimum age a tag-matching sandbox must reach before `reconcile()` will reap
    /// it. MUST exceed your longest expected run plus any clock skew between this
    /// host and the controller host, or a long legitimate run could be reaped.
    /// Default: 300s (`DEFAULT_REAP_AGE`), 10× the default 30s exec timeout.
    pub fn reap_age(self, age: Duration) -> Self;

    /// Finish into the concrete `ForkdBackend` (needed to call `reconcile()`, which
    /// the `Arc<dyn ExecutionBackend>` from `build()` cannot see).
    pub fn build_backend(self) -> Result<ForkdBackend, ForkdError>;
}

impl ForkdBackend {
    /// List the controller's sandboxes, reap tag-matching orphans older than
    /// `reap_age`, and report what was scanned / reaped / failed / skipped.
    /// Best-effort per sandbox; only a failed LIST is an error.
    pub async fn reconcile(&self) -> Result<ReconcileReport, ToolError>;
}
```

`build()` keeps its existing signature (`Result<Arc<dyn ExecutionBackend>, ForkdError>`)
for the common case — no source breakage. The current **private** `into_backend()`
is renamed to the **public** `build_backend()`; `build()` becomes
`self.build_backend().map(|b| Arc::new(b) as Arc<dyn ExecutionBackend>)`; the
in-module tests update their `into_backend()` calls to `build_backend()`.

### 4.1 Reaching `reconcile()` — the canonical handle pattern (MAJOR-3 fix)

`reconcile()` is unreachable from the `Arc<dyn ExecutionBackend>` that `build()`
returns. Operators who want GC build the **concrete** backend, wrap it in an `Arc`
**once**, and coerce a clone for the tool:

```rust
use std::sync::Arc;
use paigasus_helikon_tools::{ExecutionBackend, ForkdBackend};

let backend: Arc<ForkdBackend> = Arc::new(
    ForkdBackend::builder(url).bearer_token(t).snapshot(s).build_backend()?,
);
let shared: Arc<dyn ExecutionBackend> = backend.clone(); // hand to BashTool (unsizing coercion)
// …later, from a cron / shutdown hook:
let report = backend.reconcile().await?;                 // GC via the concrete Arc
```

`Arc<ForkdBackend>` clones by refcount (no `ForkdBackend: Clone` needed) and coerces
to `Arc<dyn ExecutionBackend>` on assignment. This pattern is the one documented in
the README and book.

### 4.2 `ReconcileReport`

```rust
#[derive(Debug, Clone)]
#[non_exhaustive]                       // matches ExecRequest/ExecOutput/SandboxGuarantees
pub struct ReconcileReport {
    /// Total sandboxes the controller LIST returned (across *all* tags) — pure
    /// observability into how busy the host is, independent of the reap set.
    pub scanned: usize,
    /// Ids successfully reaped (DELETE 2xx, or 404 = already gone → idempotent).
    pub reaped: Vec<String>,
    /// Ids that matched + were old enough but whose DELETE errored (non-404).
    /// Best-effort; non-fatal.
    pub failed: Vec<String>,
    /// Tag-matching entries whose `created_at_unix` was absent/unparseable, so we
    /// could not age them and did **not** reap. A high value with empty `reaped`
    /// is the loud signal that the wire contract drifted (BLOCKER-2 guard).
    pub skipped_unageable: usize,
}
```

Re-exported from `crates/paigasus-helikon-tools/src/lib.rs` (see §11). The reap
candidate set size is `reaped.len() + failed.len()`; `scanned >= reaped.len() +
failed.len() + skipped_unageable` (other-tag entries make it `>`).

## 5. Wire types & algorithm

### 5.1 List response — an explicitly-unverified contract

**Source & status.** The SMA-447 ticket names the endpoint (`GET /v1/sandboxes`,
forkd CLI `forkd ls`); the item field set (`id, snapshot_tag, guest_addr, pid,
memory_limit_mib, created_at_unix`) is forkd's documented fork-response shape from
`docs/API.md` (v0.5.2) as recorded in SMA-416 spike §7. **Caveat (BLOCKER-1):** that
§7 table documents Fork/Exec/Destroy/Health only — it has **no LIST row** — and no
captured LIST/fork wire body exists anywhere in this repo. So the LIST item shape
(bare array of fork-shaped items, carrying `created_at_unix` as integer **seconds**)
is an **assumption**, on the same live-only validation footing as the rest of the
forkd path (never exercised locally or in CI; proven only on the GCP harness — see
SMA-437 spec §8 and the runbook). The client codes to a **bare array** (consistent
with the fork response, which §7 confirms is a bare array even for `n:1`).

We deserialize only what we need, ignoring the rest:

```rust
#[derive(serde::Deserialize)]
struct SandboxListEntry {
    id: String,
    snapshot_tag: String,
    #[serde(default)]
    created_at_unix: Option<u64>,   // missing/unparseable ⇒ counted skipped_unageable, never reaped
}
```

`created_at_unix` is `Option` so a single odd entry can't fail the whole decode —
**but** because a wrong field *name/type/unit* would make **every** entry `None` and
silently reap nothing, the un-ageable count is surfaced on the report
(`skipped_unageable`) rather than swallowed. The live harness (§8.2) and runbook
(§12) are the gate that confirms the real field name/type/unit before GC is relied on
in production.

### 5.2 `reconcile()` steps

1. `GET {base}/v1/sandboxes` with `bearer_auth`, wrapped in `CONTROL_TIMEOUT` (10s),
   via a new `get_json` helper that **mirrors `post_json` verbatim** (same non-2xx →
   `ToolError::Other("forkd controller returned HTTP {status}")`, same decode-error
   arm, same token-free error text). `scanned = list.len()`.
2. `now =` `SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)`
   (the `unwrap_or(0)` pins the impossible pre-epoch clock to "nothing is old").
3. Partition the list:
   - not `snapshot_tag == self.snapshot` ⇒ ignore (counts only toward `scanned`).
   - tag matches but `created_at_unix` is `None` ⇒ `skipped_unageable += 1`.
   - tag matches and `now.saturating_sub(t) > reap_age_secs` (**strict `>`**) ⇒ reap
     candidate. (Strictly newer-or-equal stays; see §5.3.)
4. Reap candidates concurrently, **bounded to `REAP_CONCURRENCY` (8) in flight**
   (`futures_util::stream::iter(..).map(..).buffer_unordered(8)`), each `DELETE` under
   `CONTROL_TIMEOUT`. `try_destroy(&id)` returns `Ok` for 2xx **or 404** (already
   gone — idempotent under concurrent/repeat sweeps), `Err` otherwise → `failed`.
5. Return `ReconcileReport { scanned, reaped, failed, skipped_unageable }`.

`destroy()` is refactored: the new `try_destroy(&id) -> Result<(), ToolError>` carries
the outcome (used by `reconcile`); the existing `destroy(&id)` calls it and discards
the result, so `run()`'s teardown path is behavior-identical.

**Worst-case latency** (degraded controller, every call hitting its timeout): about
`CONTROL_TIMEOUT` (LIST) `+ ceil(N / 8) * CONTROL_TIMEOUT` for `N` candidates — e.g.
~80s for 50 orphans, vs ~500s if deletes were sequential. Documented on `reconcile()`.

### 5.3 Correctness argument (age-only) — safe under the documented invariant

- A healthy `run()` issues `DELETE` immediately after exec, so a tag-matching VM
  *strictly older* than `reap_age` is necessarily either (a) an orphan whose id we
  lost, or (b) a run hung far past its own exec timeout — both are leaks worth reaping.
- Concurrent runs younger than `reap_age − max_clock_skew` are protected. The check
  is `> reap_age` (strict), and `now` (this host) vs `created_at_unix` (controller
  host) are second-granularity clocks on possibly-different hosts, so a run between
  `reap_age` and `reap_age + skew` old *could* be reaped mid-flight. This is **not**
  unconditional correctness — it is safety under the **operator invariant**:
  **`reap_age` > (longest expected run + max tolerated clock skew)**. The default
  300s gives a 10× margin over the default 30s exec timeout for ordinary NTP skew.
- No shared in-memory state ⇒ multiple SDK processes sharing one controller + tag
  reconcile safely; the only shared keys are the snapshot tag and the wall clock.

## 6. Error handling & token hygiene

- `reconcile()` returns `Err(ToolError::Other)` **only** when the LIST fails
  (unreachable / non-2xx / decode). Per-sandbox `DELETE` failures are non-fatal and
  land in `report.failed`; a `404` is treated as success (idempotent reap). Reusing
  `ToolError` (rather than a new error type) is a deliberate choice: it matches the
  module convention — construction failures are `ForkdError`, runtime/control-plane
  failures are `ToolError::Other` (as `run()` already does).
- The bearer token rides only the `Authorization` header (never URL/body), is never
  logged, and `ToolError` text carries the URL only — `get_json` and `try_destroy`
  reuse the existing `post_json`/`destroy` patterns unchanged. `ForkdError` variants
  still never embed auth material.

## 7. Comment fix (also-in-scope)

`forkd.rs:393–396` currently reads *"… SMA-437 adds GC/reconciliation."* SMA-437
**deferred** it. Repoint at **SMA-447** and state the real remedy *with its caveat*:
the lost-id orphan is reaped by the age-based `reconcile()` sweep once it ages past
`reap_age` — **provided** the controller stamps a parseable `created_at_unix`
(otherwise it is reported as `skipped_unageable`).

## 8. Testing

### 8.1 wiremock (new `tests/forkd_reconcile.rs`, `#![cfg(feature = "microvm")]`)

| Test | Setup | Assertion |
|------|-------|-----------|
| `reconcile_reaps_only_old_tag_matching` | LIST returns: old tag-match, young tag-match, old *different*-tag, old tag-match **without** `created_at_unix` | `DELETE` fires for **only** the old tag-match id (`.expect(1)` scoped mock); `reaped == [id]`, `scanned == 4`, `failed == []`, `skipped_unageable == 1` |
| `reconcile_list_failure_is_error` | LIST → 500 | `reconcile()` → `Err`, message contains `HTTP 500`, no token in text |
| `reconcile_delete_failure_is_nonfatal` | one old tag-match; its `DELETE` → 500 | result `Ok`; id in `failed`, `reaped == []` |
| `reconcile_already_gone_is_idempotent` | one old tag-match; its `DELETE` → 404 | result `Ok`; id in `reaped`, `failed == []` |
| `reconcile_empty_list_reaps_nothing` | LIST → `[]` | `Ok`, `scanned == 0`, `skipped_unageable == 0`, no `DELETE` mounted |
| `sandbox_list_entry_deserializes` | unit | extra fields ignored; missing `created_at_unix` → `None` |

Age is controlled deterministically: `created_at_unix = now - 600` (old) vs `now`
(young, 0s age) against the 300s default — ≥300s slack each side, no realistic-CI
flake. `now` is computed in-test from `SystemTime`. Per-id `DELETE` mocks are
scoped/`expect()`-counted to assert the *exact* reap set (the young / wrong-tag /
un-ageable entries must **not** be deleted).

### 8.2 live (env-gated, `tests/forkd_live.rs`)

`live_forkd_reconcile_is_callable`: build the concrete backend via `build_backend()`,
call `reconcile()` against a real controller, assert `Ok`, and — the BLOCKER-1 gate —
assert that after at least one prior fork the LIST actually parses (i.e. real entries
exist and are not *all* `skipped_unageable`), proving the `created_at_unix` field name
/ type / unit assumption holds on the wire. True orphan-injection (build with
`reap_age(1s)`, leak a fork, sweep, assert reaped) is a **manual** runbook step (§12),
consistent with how the live path is operated (GCP harness, not CI).

## 9. Dependencies

`futures-util` (already a workspace dep, currently dev-only in this crate) becomes an
**optional** dependency gated into the `microvm` feature
(`microvm = ["dep:reqwest", "dep:futures-util", "tokio/net", "tokio/io-util"]`) for
`buffer_unordered`. Non-`microvm` builds are unaffected.

## 10. Out of scope (YAGNI)

- **Background reconciler task / scheduler** — the operator drives `reconcile()`.
- **In-flight sandbox-id tracking** — can't catch the lost-id orphan and is wrong
  across processes; age-only supersedes it (D2).
- **Decode-path immediate reap** (D3) and **`forkd cleanup` work-dir GC** (D4).
- **Cross-process coordination** beyond the age heuristic.
- **LIST pagination / server-side filtering** — forkd's LIST has no documented query
  params or pagination; we consume the single bare-array response and filter
  client-side. If a real deployment paginates, that is a follow-up (flagged in §13).

## 11. Re-export & release

- **Re-export:** add `ReconcileReport` to the `#[cfg(feature = "microvm")]` group in
  `crates/paigasus-helikon-tools/src/lib.rs` (the same block as `ForkdBackend`,
  `ForkdBackendBuilder`, `ForkdError`) with a `///` doc comment to satisfy the
  workspace `missing_docs` / `-D warnings` docs gate. **The facade needs no change:**
  it re-exports the whole crate as a module alias (`pub use paigasus_helikon_tools as
  tools;`, gated `#[cfg(feature = "tools")]`), so `ReconcileReport` is automatically
  reachable as `paigasus_helikon::tools::ReconcileReport`. (The SMA-346 facade-drift
  caveat was about dependency-version pins, not symbol re-exports — it does not apply.)
- **Release:** additive public API on the already-published `paigasus-helikon-tools
  0.2.7`. **No `paigasus-helikon-core` API is used**, so **no core bump and no
  stub-ascend ritual**. Normal release-plz flow: one `feat(tools): SMA-447 …` PR;
  release-plz auto-patch-bumps `tools` (0.x additive = patch) and cascades the facade
  dependency pin itself (no manual facade bump — that caveat only applies when *we*
  hand-bump a sibling in-PR, which we are not).

## 12. Docs (same-PR, per CLAUDE.md)

- `crates/paigasus-helikon-tools/README.md` — "microVM GC / reconciliation (SMA-447)"
  note: the `Arc<ForkdBackend>` handle pattern, `reconcile()`, `reap_age()`, and the
  `reap_age > longest-run + skew` invariant.
- `docs/book/src/concepts/tools.md` — GC/reconciliation paragraph in the
  `ForkdBackend` section, including the invariant.
- `docs/runbooks/forkd-live-validation.md` — "Validating reconcile()" section: the
  manual orphan-injection check **and** the BLOCKER-1 contract check (capture a real
  `GET /v1/sandboxes` body, confirm `created_at_unix` is integer seconds; adjust the
  deser before relying on GC if forkd's field differs).
- Design doc (this file) + plan under `docs/superpowers/plans/` on this feature branch.

## 13. Spec-challenge resolution

| Finding | Severity | Disposition |
|---------|----------|-------------|
| LIST endpoint/field uncited + unverified | BLOCKER | **Folded** — §5.1 corrects the citation, flags the assumption, routes verification to the live harness/runbook (§8.2, §12). Cannot `curl` here (no live controller). |
| `serde(default)` silent total-skip leak | BLOCKER | **Folded** — `skipped_unageable` report field (§4.2, §5.2) makes drift loud; live test asserts not-all-skipped (§8.2). |
| `>=` + clock skew overstates correctness | MAJOR | **Folded** — strict `>`; skew-aware invariant; §5.3 reframed as safety-under-invariant. |
| Unbounded sequential sweep latency | MAJOR | **Folded** — `buffer_unordered(8)`; worst-case latency documented (§5.2); `futures-util` dep (§9). |
| `build_backend()`/`build()` usability cliff | MAJOR | **Folded** — canonical `Arc<ForkdBackend>` pattern (§4.1), documented in README/book. |
| Facade re-export load-bearing/unverified | MAJOR | **Folded (reduced)** — module-alias makes facade automatic; §11 pins only the tools-`lib.rs` doc'd re-export. |
| No per-skip breakdown | MINOR | **Folded** — `skipped_unageable`. |
| Missing `#[non_exhaustive]` | MINOR | **Folded** — added (§4.2). |
| `SystemTime` expr underspecified | MINOR | **Folded** — exact expression (§5.2 step 2). |
| Comment over-promises | MINOR | **Folded** — caveat added (§7). |
| Test determinism | MINOR | No change — margins genuinely safe. |
| `get_json` error-text parity | QUESTION | **Folded** — mirror `post_json` verbatim (§5.2 step 1, §6). |
| `ToolError` vs dedicated error | QUESTION | **Decided: keep `ToolError`** — matches `run()`'s runtime-error convention (§6). |
| Double-DELETE 404 noise | QUESTION | **Folded** — `try_destroy` treats 404 as idempotent success (§4.2, §5.2). |
| LIST pagination | (raised) | **Out of scope**, explicitly flagged as a follow-up if a real deployment paginates (§10). |

## 14. Risks & mitigations

| Risk | Mitigation |
|------|------------|
| `GET /v1/sandboxes` shape differs from the assumed bare-array-of-fork-items | Live harness + runbook are the verification gate (§8.2, §12); `skipped_unageable` surfaces a `created_at_unix` drift loudly; only `id`/`snapshot_tag`/`created_at_unix` are read, all else ignored. |
| `reap_age` set below a legitimate long run (+ skew) ⇒ a live run reaped | Documented invariant on the builder, README, book, runbook; strict `>`; default 300s = 10× default exec timeout. |
| Degraded controller ⇒ slow sweep | Bounded concurrency (8) caps worst-case latency; documented (§5.2). |
| Multi-tenant controller with other tools sharing this tag | The snapshot tag is the tenancy boundary by construction; we only ever reap our own configured tag. |
| Concurrent / repeated `reconcile()` double-DELETE | 404 treated as idempotent success ⇒ no noisy `failed` entries (§5.2). |

## 15. Files touched

- `crates/paigasus-helikon-tools/src/exec/forkd.rs` — `reap_age` field + builder method;
  `build_backend()` (public, from `into_backend`); `get_json`/`try_destroy` helpers;
  `reconcile()`; `SandboxListEntry`; `ReconcileReport`; `REAP_CONCURRENCY`/`DEFAULT_REAP_AGE`
  consts; comment repoint; in-module test updates.
- `crates/paigasus-helikon-tools/src/lib.rs` — re-export `ReconcileReport` (microvm group, doc'd).
- `crates/paigasus-helikon-tools/Cargo.toml` — `futures-util` optional dep in the `microvm` feature.
- `crates/paigasus-helikon-tools/tests/forkd_reconcile.rs` — new wiremock suite.
- `crates/paigasus-helikon-tools/tests/forkd_live.rs` — `live_forkd_reconcile_is_callable`.
- `crates/paigasus-helikon-tools/README.md` — GC/reconciliation note.
- `docs/book/src/concepts/tools.md` — GC/reconciliation paragraph.
- `docs/runbooks/forkd-live-validation.md` — "Validating reconcile()" (manual orphan + contract check).
- `docs/superpowers/specs/2026-06-23-sma-447-forkd-microvm-gc-reconciliation-design.md` (this file)
  and the plan under `docs/superpowers/plans/`.
