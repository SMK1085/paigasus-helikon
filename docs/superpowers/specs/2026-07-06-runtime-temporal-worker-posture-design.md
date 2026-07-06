# SMA-455 — runtime-temporal worker-side posture + serializable-Ctx seed

**Status:** Draft (Stage 1 spec, pending GATE 1 approval)
**Ticket:** [SMA-455](https://linear.app/smaschek/issue/SMA-455) — *runtime-temporal: worker-side permission/redaction configuration + serializable-Ctx seed*
**Related:** SMA-332 (shipped the durable runner with fixed safe defaults)
**Crate:** `paigasus-helikon-runtime-temporal` (with a small additive `paigasus-helikon-core` change — see §11)

## 1. Context & problem

SMA-332 shipped the durable Temporal runner. Every tool-call activity fabricates a
fresh `RunContext` worker-side:

```rust
// activities.rs — TypedRuntime::run_context (as shipped in SMA-332)
fn run_context(&self, cancel: CancellationToken) -> RunContext<Ctx> {
    RunContext::ephemeral((self.ctx_factory)()).with_cancel(cancel)
}
```

This context has **fixed, non-configurable** posture: `redact_output = true` (env-sourced
secrets only), built-in destructive guards on, `PermissionMode::Default`, and **no**
custom deny/allow rules, permission policy, approval handler, or `extra_secrets`. The
caller's own `RunContext` posture does **not** cross the client→worker boundary (it isn't
serializable and never travels). The SMA-332 spec (§5.8) promised an *optional* worker-side
posture configuration plus a serializable-`Ctx`-seed mechanism; v0 deliberately dropped
both. The crate's `lib.rs` "Worker-Side Posture and Security Boundary" section documents
the fixed defaults and names both as future work. **SMA-455 lands that future work.**

Two capabilities are missing:

1. **Worker-side posture configuration.** A worker operator cannot tighten (or otherwise
   configure) the security posture the activities enforce.
2. **Request-scoped caller context.** Nothing lets a client hand request-scoped data
   (tenant id, user id, auth subject, …) to the worker so the fabricated `Ctx` reflects
   the specific caller of that run.

## 2. Goals / non-goals

### Goals
- Let a worker operator configure the fabricated `RunContext`'s posture: permission mode,
  deny/allow/guard rules, permission policy, approval handler, `default_guards` toggle,
  output-redaction toggle, and `extra_secrets`.
- Let a client **optionally, explicitly** attach a serializable seed that crosses to the
  worker, from which the worker's ctx factory reconstitutes a request-scoped `Ctx`.
- Add opt-in heartbeat-aware `call_model`/`invoke_tool` activities so Temporal reclaims
  abandoned attempts of a crashed worker faster.
- Keep v0 behavior **byte-for-byte identical** when none of the new knobs are used.

### Non-goals
- Serializing the permission policy or approval handler across the wire. They are
  `Arc<dyn Trait>` and stay worker-side by design (§5).
- Per-run posture *overrides* carried in the seed. Posture is worker-static; the seed
  carries caller **data**, not posture (§5). Per-run authorization is achieved by a
  worker-registered policy reading the seeded `Ctx` (§4.6).
- Hooks / guardrails / handoffs in the durable driver (still rejected at registration).
- Live token streaming, payload codecs, claim-check offloading (unchanged, still future).

## 3. Design decisions (recorded)

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | Posture is **worker-static**, set on the worker builder. | None of `PermissionMode`/`DenyRule`/`AllowRule`/`GuardRule` derive `Serialize`; `PermissionPolicy`/`ApprovalHandler` are trait objects. Posture *cannot* cross the wire. Forced, not chosen. |
| D2 | Seed is a **type-erased `serde_json::Value`**; the seeded factory is `Fn(Option<Value>) -> Ctx`. | Keeps the worker's careful `Ctx`-erasure intact (no second generic on the builder/runner). Matches the JSON wire boundary. The factory owns typed deserialization. |
| D3 | Seed delivery is **config-level**: `TemporalRunnerConfig::with_ctx_seed(Value)`. | *(User decision.)* Smallest surface; works through the fixed `Runner` trait unchanged, incl. `dyn Runner`. Request-scope = construct a runner (or clone the config) per request; the client is cheaply cloneable. |
| D4 | Posture knobs are grouped into a **`WorkerPosture<Ctx>`** builder, set via one `TemporalAgentWorkerBuilder::posture(...)` call. | 9 knobs would bloat the worker builder; grouping keeps the security surface one reviewable unit (mirrors core's internal `PermissionFields`). `WorkerPosture::default()` == exact v0 defaults. |
| D5 | **Heartbeats included** (opt-in), default off. | *(User decision.)* Emits `record_heartbeat` during in-flight `call_model`/`invoke_tool` via a background ticker + `heartbeat_timeout` on the activity options. Off by default = current behavior preserved. |
| D6 | This release is **replay-breaking** and must be flagged. | The activity arg tuples gain a `ctx_seed` element (arity change) and model/tool `ActivityOptions` gain `heartbeat_timeout`. In-flight histories won't replay against the new worker — the crate already documents drain-before-upgrade / blue-green queues. |

## 4. Architecture

All changes are in `paigasus-helikon-runtime-temporal` except a tiny additive core change
(§11). No existing public signature is removed; `with_ctx` and the current defaults stay.

### 4.1 `WorkerPosture<Ctx>` (new public type, `worker.rs`)

A grouped, `Ctx`-generic builder bundling the posture core's `RunContext` already exposes:

```rust
pub struct WorkerPosture<Ctx> {
    permission_mode: PermissionMode,
    deny_rules: Vec<DenyRule>,
    allow_rules: Vec<AllowRule>,
    guard_rules: Vec<GuardRule>,
    permission_policy: Option<Arc<dyn PermissionPolicy<Ctx>>>,
    approval_handler: Option<Arc<dyn ApprovalHandler>>,
    default_guards: bool,      // default true
    redact_output: bool,       // default true
    extra_secrets: Vec<String>,
}
```

- `Default` yields **exactly** the v0 fixed defaults (mode `Default`, `default_guards =
  true`, `redact_output = true`, empty rule/secret vectors, no policy/handler).
- Chainable setters: `with_permission_mode`, `with_deny_rules`, `with_allow_rules`,
  `with_guard_rules`, `with_permission_policy`, `with_approval_handler`,
  `without_default_guards`, `without_output_redaction`, `with_extra_secrets`.
- One consumer method (`pub(crate)`):
  ```rust
  fn apply(&self, ctx: RunContext<Ctx>) -> RunContext<Ctx>
  ```
  which chains the matching `RunContext::with_*` builder calls, cloning the `Arc`
  handles and rule/secret `Vec`s (cheap; a fresh `RunContext` is built per activity
  invocation). Applying `with_permission_mode(mode)` onto a fresh `Default` context is a
  legal tighten in every case (`Default → X` is always allowed by core's tighten-only
  rule).

### 4.2 `TemporalAgentWorkerBuilder<Ctx>` additions (`worker.rs`)

New fields + builder methods (all optional; omitting them preserves v0):

- `posture: WorkerPosture<Ctx>` (default `WorkerPosture::default()`), set via
  `.posture(WorkerPosture<Ctx>)`.
- `ctx_factory: Arc<dyn Fn(Option<serde_json::Value>) -> Ctx + Send + Sync>` — the
  existing `with_ctx(Fn() -> Ctx)` now wraps its nullary closure into this shape
  (ignoring the seed); a new `with_seeded_ctx(Fn(Option<Value>) -> Ctx)` sets it
  directly. Both satisfy the `MissingCtxFactory` build check; they set the same slot
  (last-wins).
- `heartbeat_interval: Option<Duration>` (default `None` = off), set via
  `.heartbeat_interval(Duration)`.

`build()` threads:
- the posture and `heartbeat_interval` into `build_activities(...)` → the `TypedRuntime`
  (posture) / `AgentActivities` (heartbeat interval);
- `heartbeat_interval` into `ActivityTimeouts`/`build_activity_config` so the model/tool
  `ActivityOptions` get `.maybe_heartbeat_timeout(...)` (§4.5).

### 4.3 Activity-side application (`activities.rs`)

`TypedRuntime<Ctx>` gains a `posture: WorkerPosture<Ctx>` field and its `ctx_factory`
becomes `Fn(Option<Value>) -> Ctx`. `run_context` takes the per-run seed:

```rust
fn run_context(&self, seed: Option<serde_json::Value>, cancel: CancellationToken)
    -> RunContext<Ctx>
{
    let ctx = RunContext::ephemeral((self.ctx_factory)(seed)).with_cancel(cancel);
    self.posture.apply(ctx)
}
```

`DurableAgentRuntime::render_instructions` and `invoke_tool` gain a `ctx_seed:
Option<Value>` parameter and pass it to `run_context`. `call_model` is **unchanged** — it
never builds a `RunContext` (it calls `call_model_inner(def.model, …)` directly), so the
seed does not reach it. The three inner functions (`*_inner`) are untouched.

`AgentActivities` gains a `heartbeat_interval: Option<Duration>` field (from
`build_activities`). The `#[activity]` methods thread the seed and (for `call_model` /
`invoke_tool`) run the heartbeat ticker (§4.5).

### 4.4 Seed threading across the boundary

- **`payloads.rs`** — `WorkflowInput` gains:
  ```rust
  #[serde(default)]
  pub ctx_seed: Option<serde_json::Value>,
  ```
  `#[serde(default)]` so any `WorkflowInput` serialized before this change still
  deserializes (defensive; drain-before-upgrade remains the real guarantee).
- **`runner.rs`** — `TemporalRunnerConfig` gains `ctx_seed: Option<Value>` (default
  `None`) + `with_ctx_seed(Value)`. `run_inner` sets `WorkflowInput.ctx_seed =
  self.config.ctx_seed.clone()`. No `Runner` trait change.
- **`workflow.rs`** — `drive` extracts `input.ctx_seed` and threads a clone into
  `run_effects` / `execute_tools`, which pass it in the `render_instructions` and
  `invoke_tool` `start_activity` argument tuples. The seed is deterministic (constant
  input data, identical across replay), so this is replay-safe. `call_model`'s tuple is
  unchanged.

**Payload cost:** the seed is serialized into history on *every* `render_instructions` /
`invoke_tool` activity call (once each per turn/tool). Keep it small (ids, claims), not
bulk data — documented in the payload-budget section.

### 4.5 Heartbeats (`activities.rs` + `workflow.rs`)

- **Workflow side:** when `heartbeat_interval` is `Some(iv)`, `build_activity_config`
  sets `heartbeat_timeout = 2 × iv` (with a small floor) on the **model** and **tool**
  `ActivityOptions` via `.maybe_heartbeat_timeout(...)`. `render_instructions` gets no
  heartbeat (fast, no network). When `None`, no heartbeat_timeout is set (v0 behavior).
- **Activity side:** `call_model` / `invoke_tool` add a heartbeat branch to the existing
  `race_with_activity_cancellation` `tokio::select!`: a `tokio::time::interval(iv)` tick
  calls `activity_ctx.record_heartbeat(Default::default())` and loops, running until the
  work future completes. Because the ticker heartbeats while the process is alive, a
  genuinely long single call never spuriously heartbeat-times-out; only a **dead worker**
  (ticker stopped) trips `heartbeat_timeout`, letting Temporal reclaim/re-dispatch per
  the activity's retry policy — the crash-reclamation win. The ticker interval (`iv`) is
  strictly less than the timeout (`2 × iv`), giving margin.

Safety: heartbeat timeout only fires when the worker is actually dead, so it never
introduces a spurious retry that would violate tool idempotency expectations beyond what
`start_to_close` already implies.

### 4.6 The elegant composition (why the seed needs no posture)

A worker registers a `PermissionPolicy<Ctx>` (trait object, worker-static). At tool-call
time, `run_context` builds a `Ctx` **from the per-run seed**, so the policy's
`check(ctx, tool, args)` can read `ctx.user_ctx()` (the seeded value) and make
**request-scoped** authorization decisions — e.g. "tenant `acme` may run `Bash`, others
may not." This delivers the "finer-grained permission inheritance" the `lib.rs` note
wants, **without ever serializing the policy**: static policy + dynamic seed = dynamic
per-run authorization. This is the reason posture-in-the-seed is a non-goal (§2).

## 5. Security model

- **Worker posture is authoritative.** The worker operator — not the client — controls
  what tool calls are permitted. A client's seed is **data**, never posture; it cannot
  loosen (or set) mode/rules/policy/handler. This preserves SMA-332's security-boundary
  story.
- **The seed is an explicit, opt-in trust hand-off.** A worker that calls
  `with_seeded_ctx` chooses to trust seed contents from clients on its task queue; a
  worker using `with_ctx` ignores any seed entirely. Document that the seed is
  attacker-influenced iff untrusted clients can start workflows on the queue.
- **Temporal history is a persistence boundary.** The seed is recorded in history
  (per activity call). **Do not put secrets in the seed.** Tool outputs are still
  redacted *before* entering history when `redact_output = true`; a worker that calls
  `without_output_redaction()` writes unredacted output into permanent history — call
  this out loudly in docs.
- `extra_secrets` now configurable worker-side *improves* redaction coverage (values
  known to the operator but not env-sourced).

## 6. Public API surface (additive; nothing removed)

`paigasus-helikon-runtime-temporal`:
- `worker::WorkerPosture<Ctx>` + its setters and `Default`.
- `TemporalAgentWorkerBuilder::posture(WorkerPosture<Ctx>)`.
- `TemporalAgentWorkerBuilder::with_seeded_ctx(Fn(Option<Value>) -> Ctx)`.
- `TemporalAgentWorkerBuilder::heartbeat_interval(Duration)`.
- `runner::TemporalRunnerConfig::with_ctx_seed(Value)` + public `ctx_seed` field.
- `payloads::WorkflowInput::ctx_seed` (public field, `#[serde(default)]`).

Re-exports of core posture types (`PermissionMode`, `DenyRule`, `AllowRule`, `GuardRule`,
`PermissionPolicy`, `ApprovalHandler`) as needed so users can build a `WorkerPosture`
without a direct `paigasus-helikon-core` dependency (verify what's already re-exported;
add only the gaps). Every new `pub` item carries a `///` doc comment (doc-coverage gate).

## 7. Testing strategy (TDD)

Unit (no Temporal server, fast):
- `WorkerPosture::default()` applied to a fresh context reproduces v0 (`permission_mode ==
  Default`, `default_guards`, `redact_output`, empty rules).
- `WorkerPosture::apply` installs each knob: a deny rule denies; `Plan` mode blocks a
  write; `without_output_redaction` clears the flag; `extra_secrets` accumulate; an
  approval handler + a guard `Ask` resolves to Allow.
- `invoke_tool_inner` through a `TypedRuntime` with a posture denies a denied tool
  (outcome carries the denial string).
- Seeded factory: `with_seeded_ctx` receives `Some(seed)`; `with_ctx` ignores the seed;
  a `None` seed (no `with_ctx_seed`) reaches the factory as `None`.
- Request-scoped policy: a `PermissionPolicy` reading `ctx.user_ctx()` allows/denies by
  seed content (the §4.6 composition).
- `WorkflowInput` round-trips with and without `ctx_seed`; a legacy JSON object lacking
  the field deserializes (`#[serde(default)]`).
- `build_activity_config` sets `heartbeat_timeout` on model/tool opts when interval is
  `Some`, and leaves it unset when `None`; `render_instructions` never gets one.

Live (env-gated `temporal_live.rs`, loud-skip without a server — mirrors SMA-332):
- A run with a configured seed + a request-scoped policy denies/allows the expected tool.
- (Best-effort / documented) heartbeat reclamation: a killed worker mid-`invoke_tool`
  is re-dispatched faster with a short `heartbeat_interval` than without. If not
  reliably automatable, cover the wiring by unit-asserting `heartbeat_timeout` and note
  the manual validation step in the runbook.

## 8. Docs to update (same PR)

- `runtime-temporal/src/lib.rs` — rewrite "Worker-Side Posture and Security Boundary":
  from "fixed defaults / future work" to "configurable via `WorkerPosture`; defaults
  unchanged; seed mechanism; heartbeats". Add the seed to the payload-budget note and the
  determinism/upgrade section (replay-breaking flag).
- `runtime-temporal/README.md` — posture + seed usage snippet; note published-surface
  changes.
- `docs/superpowers/specs/2026-07-05-runtime-temporal-agentcore-design.md` §5.8 as-built
  note — mark the worker-side configuration and Ctx-seed as **landed in SMA-455** (update
  the two lines that call them future work).
- `docs/book/src/concepts/runtimes.md` — if it documents the fixed-defaults posture,
  bring it in line.
- `CHANGELOG.md` (runtime-temporal) — features + **replay-breaking** flag (D6).

## 9. Release mechanics

`runtime-temporal` is already a released crate (ascended in SMA-332 via #136/#137), so it
ships through release-plz's normal flow — no stub-ascend ritual. The core change (§11) is
additive; bump `paigasus-helikon-core` a patch + its workspace pin + CHANGELOG **only if**
§11 turns out necessary (see below). Confirm whether the facade needs a bump per the
CLAUDE.md cascade rules once the exact version deltas are known.

## 10. Out of scope / future work

- Per-run posture overrides in the seed.
- Serializable permission policy / approval handler.
- Payload codecs, claim-check blob offload, conversation compaction (still named future).
- Temporal Worker Versioning (Build IDs) for zero-downtime replay-breaking upgrades.

## 11. Core dependency check (may be a no-op)

The plan is to build `WorkerPosture` entirely from **already-public** core APIs
(`RunContext::with_permission_mode` / `with_deny_rules` / `with_allow_rules` /
`with_guard_rules` / `with_permission_policy` / `with_approval_handler` /
`without_default_guards` / `without_output_redaction` / `with_extra_secrets`, all
confirmed `pub` in `context.rs`). If so, **no core change is needed** and the whole
feature is contained in `runtime-temporal`. A core bump + workspace-pin + facade bump is
required **only** if implementation surfaces a missing re-export or API (e.g. a posture
type not yet re-exported from core's prelude) — decided during implementation, following
the CLAUDE.md same-PR-core-bump caveat if it arises.
