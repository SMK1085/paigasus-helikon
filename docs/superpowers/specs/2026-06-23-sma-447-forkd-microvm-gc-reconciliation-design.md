# SMA-447 — forkd microVM: GC / reconciliation of orphaned sandboxes — design

**Ticket:** [SMA-447](https://linear.app/smaschek/issue/SMA-447/paigasus-helikon-tools-forkd-microvm-gcreconciliation-of-orphaned)
**Branch:** `feature/sma-447-paigasus-helikon-tools-forkd-microvm-gcreconciliation-of`
**Follows:** SMA-416 (skeleton that first named the gap), SMA-437 (egress + live validation; consciously re-deferred this in its spec §10).
**Crate:** `paigasus-helikon-tools` (already published, `0.2.7`), file `crates/paigasus-helikon-tools/src/exec/forkd.rs`, feature `microvm`.

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

**Non-goals (YAGNI — see §9):** a background sweeper task, in-flight id tracking, an
immediate decode-path reap, `forkd cleanup` work-dir GC, and any cross-process
coordination beyond the age heuristic.

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
    /// it. MUST exceed your longest expected run, or a long legitimate run could be
    /// reaped. Default: 300s (`DEFAULT_REAP_AGE`), 10× the default 30s exec timeout.
    pub fn reap_age(self, age: Duration) -> Self;

    /// Finish into the concrete `ForkdBackend` (needed to call `reconcile()`, which
    /// the `Arc<dyn ExecutionBackend>` from `build()` cannot see). `build()` is
    /// `build_backend().map(Arc::new)`.
    pub fn build_backend(self) -> Result<ForkdBackend, ForkdError>;
}

impl ForkdBackend {
    /// List the controller's sandboxes, reap tag-matching orphans older than
    /// `reap_age`, and report what was listed / reaped / failed. Best-effort per
    /// sandbox; only a failed LIST is an error.
    pub async fn reconcile(&self) -> Result<ReconcileReport, ToolError>;
}
```

`build()` keeps its existing signature (`Result<Arc<dyn ExecutionBackend>, ForkdError>`)
for the common case — no source breakage. The current **private** `into_backend()`
is renamed to the **public** `build_backend()`; the in-module tests update their
`into_backend()` calls accordingly.

### 4.1 `ReconcileReport`

```rust
#[derive(Debug, Clone)]
pub struct ReconcileReport {
    /// Total sandboxes the controller LIST returned (across *all* tags) — pure
    /// observability into how busy the host is, independent of the reap set.
    pub scanned: usize,
    /// Ids successfully `DELETE`d (orphans of *this* tag older than `reap_age`).
    pub reaped: Vec<String>,
    /// Ids that matched but whose `DELETE` failed (best-effort; non-fatal).
    pub failed: Vec<String>,
}
```

Re-exported from `crates/paigasus-helikon-tools/src/lib.rs` alongside `ForkdBackend`.
The reap candidate set size is `reaped.len() + failed.len()`; `scanned` is the
full LIST size (`scanned >= reaped.len() + failed.len()`).

## 5. Wire types & algorithm

### 5.1 List response

`GET /v1/sandboxes` returns the **same item shape** as the fork response
(SMA-416 spike §7): `[{"id","snapshot_tag","guest_addr","pid","memory_limit_mib","created_at_unix"}]`.
We deserialize only what we need, ignoring the rest:

```rust
#[derive(serde::Deserialize)]
struct SandboxListEntry {
    id: String,
    snapshot_tag: String,
    #[serde(default)]
    created_at_unix: Option<u64>,
}
```

`created_at_unix` is `Option` on purpose: a missing/null value means we **cannot
prove the VM is old**, so it is **skipped** (never reap what you can't age).

### 5.2 `reconcile()` steps

1. `GET {base}/v1/sandboxes` with `bearer_auth`, wrapped in `CONTROL_TIMEOUT` (10s),
   via a new `get_json` helper mirroring `post_json`. Non-2xx → `Err`; decode fail → `Err`.
2. `now =` `SystemTime::now()` → unix seconds (`UNIX_EPOCH` elapsed; on the
   theoretically-impossible pre-epoch clock, treat as `0` ⇒ nothing is "old" ⇒ no reap).
3. **Candidate** iff `entry.snapshot_tag == self.snapshot` **AND** `created_at_unix`
   is `Some(t)` **AND** `now.saturating_sub(t) >= reap_age_secs`.
4. For each candidate: best-effort `DELETE /v1/sandboxes/:id` (bearer, each wrapped in
   `CONTROL_TIMEOUT`). Success → `reaped`; any failure/timeout → `failed`.
5. Return `ReconcileReport { scanned, reaped, failed }` where `scanned` is the full
   LIST length and `reaped`/`failed` are the per-candidate `DELETE` outcomes.

`destroy()` is refactored: a new `try_destroy(&id) -> Result<(), ToolError>` returns
the outcome (used by `reconcile`); the existing `destroy(&id)` calls it and discards
the result, so `run()`'s teardown path is byte-for-byte unchanged in behavior.

### 5.3 Correctness argument (age-only)

- A healthy `run()` issues `DELETE` immediately after exec, so a tag-matching VM
  older than `reap_age` is necessarily either (a) an orphan whose id we lost, or
  (b) a run hung far past its own exec timeout — both are leaks worth reaping.
- Concurrent **fresh** runs are younger than `reap_age` ⇒ protected.
- No shared in-memory state ⇒ multiple SDK processes sharing one controller + tag
  reconcile safely; the only shared keys are the snapshot tag and the wall clock.
- **Operator invariant:** `reap_age` > longest expected run. Documented on
  `reap_age()`, in the README, the book, and the runbook.

## 6. Error handling & token hygiene

- `reconcile()` returns `Err(ToolError::Other)` **only** when the LIST fails
  (unreachable / non-2xx / decode). Per-sandbox `DELETE` failures are non-fatal and
  land in `report.failed`, mirroring the best-effort philosophy already in `destroy()`.
- The bearer token rides only the `Authorization` header (never URL/body), is never
  logged, and `ToolError` text carries the URL only — unchanged from the existing
  `post_json`/`destroy` patterns. `ForkdError` variants still never embed auth material.

## 7. Comment fix (also-in-scope)

`forkd.rs:393–396` currently reads *"… SMA-437 adds GC/reconciliation."* SMA-437
**deferred** it. Repoint at **SMA-447** and state the real remedy: the lost-id orphan
is reaped by the age-based `reconcile()` sweep once it ages past `reap_age`.

## 8. Testing

### 8.1 wiremock (new `tests/forkd_reconcile.rs`, `#![cfg(feature = "microvm")]`)

| Test | Setup | Assertion |
|------|-------|-----------|
| `reconcile_reaps_only_old_tag_matching` | LIST returns: old tag-match, young tag-match, old *different*-tag, entry with no `created_at_unix` | `DELETE` fires for **only** the old tag-match id; `reaped == [id]`, `scanned == 4`, `failed == []` |
| `reconcile_list_failure_is_error` | LIST → 500 | `reconcile()` → `Err`, message contains `HTTP 500`, no token in text |
| `reconcile_delete_failure_is_nonfatal` | one old tag-match; its `DELETE` → 500 | result `Ok`; id in `failed`, `reaped == []` |
| `reconcile_empty_list_reaps_nothing` | LIST → `[]` | `Ok`, `scanned == 0`, no `DELETE` mounted |
| `sandbox_list_entry_deserializes` | unit | extra fields ignored; missing `created_at_unix` → `None` |

Age is controlled deterministically: `created_at_unix = now - 600` (old) vs `now`
(young) against the 300s default — 2× margins each side, no flake. `now` is computed
in-test from `SystemTime`. Per-id `DELETE` mocks are scoped/`expect()`-counted to
assert the *exact* reap set (no over-reaping of the young / wrong-tag / un-ageable
entries).

### 8.2 live (env-gated, `tests/forkd_live.rs`)

Add `live_forkd_reconcile_is_callable`: build the concrete backend via
`build_backend()`, assert `reconcile()` returns `Ok` against a real controller and
log the report. True orphan-injection (build with `reap_age(1s)`, leak a fork, sweep,
assert reaped) is documented as a **manual** runbook step rather than an automated
live test, consistent with how the live path is operated (GCP harness, not CI).

## 9. Out of scope (YAGNI)

- **Background reconciler task / scheduler** — the operator drives `reconcile()`.
- **In-flight sandbox-id tracking** — can't catch the lost-id orphan and is wrong
  across processes; age-only supersedes it (D2).
- **Decode-path immediate reap** (D3) and **`forkd cleanup` work-dir GC** (D4).
- **Cross-process coordination** beyond the age heuristic.
- **Filtering server-side** — forkd's LIST has no documented tag/age query params;
  we filter client-side.

## 10. Risks & mitigations

| Risk | Mitigation |
|------|------------|
| `reap_age` set below a legitimate long run ⇒ a live run reaped | Documented invariant (`reap_age` > longest run) on the builder, README, book, runbook; default 300s is 10× the default exec timeout. |
| forkd LIST item shape drifts from the fork-response shape | We deserialize only `id` + `snapshot_tag` + optional `created_at_unix` and ignore the rest; missing `created_at_unix` fails safe (skip). REST boundary already insulated us (SMA-416 §3). |
| Multi-tenant controller with VMs from *other* tools sharing this tag | Out of band: the snapshot tag is the tenancy boundary by construction; we only ever reap our own configured tag. |
| `created_at_unix` clock skew between controller host and SDK host | Age is computed on the SDK's clock vs the controller's stamp; large skew could mis-age. Accepted — same trust domain in the documented single-host/runbook deployment; default 300s absorbs ordinary NTP skew. |

## 11. Release / versioning

Additive public API on the already-published `paigasus-helikon-tools 0.2.7`. **No
`paigasus-helikon-core` API is used**, so **no core bump and no stub-ascend ritual**.
Normal release-plz flow: a single `feat(tools): SMA-447 …` PR; release-plz
auto-patch-bumps `tools` (0.x additive = patch) and cascades the facade dependency
pin itself (no manual facade bump — that caveat only applies when *we* hand-bump a
sibling in-PR, which we are not). `ReconcileReport` must be re-exported from the tools
`lib.rs`; the facade re-export style is verified during implementation.

## 12. Files touched

- `crates/paigasus-helikon-tools/src/exec/forkd.rs` — `reap_age` field + builder method;
  `build_backend()` (public, from `into_backend`); `get_json`/`try_destroy` helpers;
  `reconcile()`; `SandboxListEntry`; comment repoint; in-module test updates.
- `crates/paigasus-helikon-tools/src/lib.rs` — re-export `ReconcileReport`.
- `crates/paigasus-helikon-tools/tests/forkd_reconcile.rs` — new wiremock suite.
- `crates/paigasus-helikon-tools/tests/forkd_live.rs` — `live_forkd_reconcile_is_callable`.
- `crates/paigasus-helikon-tools/README.md` — "microVM GC / reconciliation (SMA-447)" note.
- `docs/book/src/concepts/tools.md` — GC/reconciliation paragraph in the `ForkdBackend` section.
- `docs/runbooks/forkd-live-validation.md` — "Validating reconcile()" manual orphan-injection check.
- `docs/superpowers/specs/2026-06-23-sma-447-forkd-microvm-gc-reconciliation-design.md` (this file)
  and the plan under `docs/superpowers/plans/`.
