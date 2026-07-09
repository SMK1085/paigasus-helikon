# SMA-455 — runtime-temporal worker-side posture + serializable-Ctx seed

**Status:** Draft (Stage 1 spec, pending GATE 1 approval)
**Ticket:** [SMA-455](https://linear.app/smaschek/issue/SMA-455) — *runtime-temporal: worker-side permission/redaction configuration + serializable-Ctx seed*
**Related:** SMA-332 (shipped the durable runner with fixed safe defaults)
**Crate:** `paigasus-helikon-runtime-temporal` (self-contained; a `paigasus-helikon-core` change is likely **unnecessary** — see §11)

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
| D6 | Wire-input additions are **backward-compatible-by-construction**; the upgrade posture is validated, not assumed. | *(Revised after challenge.)* `WorkflowInput.ctx_seed` is `#[serde(default)]` (old payloads deserialize; serde ignores unknown fields on rollback). Whether **re-scheduling** an activity with a changed input tuple (or added `heartbeat_timeout`) trips the Rust SDK's non-determinism checker is **validated during implementation** via the live test — completed activities replay from recorded results, so only newly-scheduled activities use the new shape. The crate's existing drain-before-upgrade / blue-green-queue discipline remains the conservative guidance regardless. **Do not assert "arity change → non-determinism" without SDK evidence.** |
| D7 | The seeded factory is **fallible** at its core; a bad seed fails the run **loud and fast**, never silently defaults. | *(Added after challenge — fixes the BLOCKER.)* The internal factory slot is `Fn(Option<Value>) -> Result<Ctx, E>`. A seed-deserialize error maps to a **non-retryable** `ApplicationFailure`, so a hostile/malformed seed fails the run immediately instead of panicking into Temporal's default **unlimited retry** loop (verified: the SDK wraps activity bodies in `catch_unwind` → *retryable* failure, and `render_instructions` carries no retry policy). Silently substituting a default `Ctx` is rejected: it would authorize the run under the **wrong** caller identity. |

## 4. Architecture

All changes are expected to be contained in `paigasus-helikon-runtime-temporal`; a
`paigasus-helikon-core` change is likely **unnecessary** (§11 states the one condition —
a missing API/re-export — under which one would be needed). No existing public signature
is removed; `with_ctx` and the current defaults stay.

### 4.1 `WorkerPosture<Ctx>` (new public type, `worker.rs`)

A grouped, `Ctx`-generic builder bundling the nine posture knobs core's `RunContext`
already exposes:

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
  - **The boolean toggles are one-way.** Core exposes only `without_default_guards()` /
    `without_output_redaction()` (no `with_*(true)` inverse). Because a fresh
    `RunContext::ephemeral` already defaults both to `true`, `apply` calls each
    *conditionally*: `if !self.default_guards { ctx = ctx.without_default_guards() }`,
    likewise for redaction. It is not a symmetric setter chain.

**Deliberate-tech-debt note (drift risk).** `WorkerPosture` is the **fifth** hand-copy of
core's nine-field permission bundle (alongside `RunContext`'s fields, `PermissionFields`,
and the two child-context copy sites the project memo already flags). `PermissionFields`
is `pub(crate)` in core, so it cannot be reused across the crate boundary — hence the
duplication. When core gains a tenth knob, the durable runtime will silently be unable to
configure it. This is accepted for now and **called out in the code**; the real de-dup is
a future public `PermissionConfig` value-type in core (out of scope here). The §7
default-equivalence test guards against *today's* drift by asserting **every** field (see
§7).

### 4.2 `TemporalAgentWorkerBuilder<Ctx>` additions (`worker.rs`)

New fields + builder methods (all optional; omitting them preserves v0):

- `posture: WorkerPosture<Ctx>` (default `WorkerPosture::default()`), set via
  `.posture(WorkerPosture<Ctx>)`.
- `ctx_factory: Arc<dyn Fn(Option<serde_json::Value>) -> Result<Ctx, CtxSeedError> + Send + Sync>`
  — a **fallible** internal slot (D7). Three public setters feed it, last-wins, all
  satisfying the `MissingCtxFactory` build check:
  - `with_ctx(Fn() -> Ctx)` — existing signature, unchanged for callers; wrapped as
    `move |_seed| Ok(f())`. Seed ignored.
  - `with_seeded_ctx(Fn(Option<Value>) -> Ctx)` — infallible seeded factory; wrapped as
    `move |seed| Ok(f(seed))`. **Totality contract:** this closure must never panic and
    must be cheap (it runs once per `render_instructions` and per `invoke_tool` — see
    §4.3); deserialize the seed defensively and fall back to a safe default only when a
    default identity is genuinely acceptable.
  - `try_with_seeded_ctx(Fn(Option<Value>) -> Result<Ctx, E>)` where `E: Display` —
    the **security-sensitive** path: a seed-deserialize error is surfaced (mapped in the
    activity to a **non-retryable** `ApplicationFailure`), so a bad seed **fails the run
    loud and fast** rather than authorizing under a wrong/default identity. Recommended
    whenever the seed drives authorization (§4.6). `CtxSeedError` wraps `E`'s `Display`.
- `heartbeat_interval: Option<Duration>` (default `None` = off), set via
  `.heartbeat_interval(Duration)`.

`build()` threads:
- the posture and `heartbeat_interval` into `build_activities(...)` → the `TypedRuntime`
  (posture) / `AgentActivities` (heartbeat interval);
- `heartbeat_interval` into `ActivityTimeouts`/`build_activity_config` so the model/tool
  `ActivityOptions` get `.maybe_heartbeat_timeout(...)` (§4.5).

### 4.3 Activity-side application (`activities.rs`)

`TypedRuntime<Ctx>` gains a `posture: WorkerPosture<Ctx>` field and its `ctx_factory`
becomes the fallible `Fn(Option<Value>) -> Result<Ctx, CtxSeedError>`. `run_context` is
therefore **fallible**, and a factory error becomes a **non-retryable** `ActivityError`:

```rust
fn run_context(&self, seed: Option<serde_json::Value>, cancel: CancellationToken)
    -> Result<RunContext<Ctx>, ActivityError>
{
    let user_ctx = (self.ctx_factory)(seed).map_err(|e| {
        ActivityError::application(ApplicationFailure::non_retryable(
            format!("ctx seed rejected: {e}"),
        ))
    })?;
    let ctx = RunContext::ephemeral(user_ctx).with_cancel(cancel);
    Ok(self.posture.apply(ctx))
}
```

`DurableAgentRuntime::render_instructions` and `invoke_tool` gain a `ctx_seed:
Option<Value>` parameter and `?`-propagate `run_context`'s error. Mapping to
**non-retryable** is what defuses the BLOCKER: without it, a factory panic/error on the
retry-policy-less `render_instructions` (or the tool activity) would retry-loop forever
(§5). `call_model` is **unchanged** — it never builds a `RunContext` (it calls
`call_model_inner(def.model, …)` directly), so the seed never reaches it. The three inner
functions (`*_inner`) are untouched.

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
- **`runner.rs`** — `TemporalRunnerConfig` gains a **private** `ctx_seed: Option<Value>`
  (default `None`) set only via the `with_ctx_seed(Value)` builder. Private, not `pub`
  like the existing fields: the struct has no `#[non_exhaustive]`, so exposing another
  public field is a struct-literal breaking change, and `new()` + `with_ctx_seed()`
  already cover construction. `run_inner` (same module) reads it into
  `WorkflowInput.ctx_seed = self.config.ctx_seed.clone()`. No `Runner` trait change.
- **`workflow.rs`** — `drive` extracts `input.ctx_seed` and threads a clone into
  `run_effects` / `execute_tools`, which pass it in the `render_instructions` and
  `invoke_tool` `start_activity` argument tuples. The seed is deterministic (constant
  input data, identical across replay), so this is replay-safe. `call_model`'s tuple is
  unchanged.

**Payload cost:** the seed is serialized into history on *every* `render_instructions` /
`invoke_tool` activity call (once each per turn/tool). Keep it small (ids, claims), not
bulk data — documented in the payload-budget section.

### 4.5 Heartbeats (`activities.rs` + `workflow.rs`)

- **Interval floor:** `heartbeat_interval(iv)` floors `iv` to a documented minimum
  (**1 s**) — a sub-second `iv` gives `heartbeat_timeout = 2 × iv` below the
  heartbeat→server round-trip latency, guaranteeing a false timeout on a healthy worker.
  Values below the floor are clamped (documented), not accepted verbatim.
- **Workflow side:** when `heartbeat_interval` is `Some(iv)`, `build_activity_config`
  sets `heartbeat_timeout = 2 × iv` on the **model** and **tool** `ActivityOptions` via
  `.maybe_heartbeat_timeout(...)`. `render_instructions` gets **no** heartbeat (fast, no
  network — it passes `None`). When `heartbeat_interval` is `None`, no `heartbeat_timeout`
  is set (v0 behavior, byte-identical).
- **Activity side — a real restructure, not a bolt-on branch.** The existing
  `race_with_activity_cancellation` (`activities.rs:255-269`) is a single `tokio::select!`
  returning `T`; a ticker branch yields `()`, not `T`, so it cannot simply be added. It
  is rewritten to a `loop`:
  ```rust
  // heartbeat: Option<Duration>; None for render_instructions and when the knob is off.
  let mut ticker = heartbeat.map(tokio::time::interval);
  tokio::pin!(work);
  loop {
      tokio::select! {
          biased;                                   // preserve: poll work/cancel first
          result = &mut work => return result,
          _ = activity_ctx.cancelled() => {         // preserve: after cancel, still
              cancel.cancel();                      // await work to completion so no
              return work.await;                    // detached task leaks (the
          }                                         // original's load-bearing guarantee)
          _ = tick(&mut ticker), if ticker.is_some() => {
              activity_ctx.record_heartbeat(Vec::new()); // liveness-only, empty details
          }
      }
  }
  ```
  `record_heartbeat(Vec::new())` sends empty details — we heartbeat purely for liveness,
  not progress-checkpointing (no resume-from-checkpoint payload). The interval (`iv`) is
  strictly below the timeout (`2 × iv`), giving margin.

**Honest safety caveat (not "dead worker only").** A heartbeat trips `heartbeat_timeout`
whenever the ticker stops polling — which is a **crashed worker** (the intended win) **but
also a live worker whose async executor is starved** by a blocking/CPU-bound tool `invoke`
(e.g. a synchronous `std::process::Command`, heavy compute with no `.await`). In that case
Temporal re-dispatches a possibly non-idempotent tool, narrowing the double-run window from
`start_to_close` (default 300 s) to `~2 × iv`. This does **not** violate ADR-10 (that
concerns `ModelError` → non-retryable, unrelated), but it *does* interact with the tool-
idempotency contract already documented in `lib.rs`. The docs (§8) must: (a) recommend
tools offload blocking work via `tokio::task::spawn_blocking`, and (b) cross-reference the
existing "tool idempotency under crash-retry" warning. Enabling heartbeats is a latency/
reclamation *tuning* choice with this trade-off, not a free win.

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
  `with_seeded_ctx` / `try_with_seeded_ctx` chooses to trust seed contents from clients on
  its task queue; a worker using `with_ctx` ignores any seed entirely. Document that the
  seed is attacker-influenced iff untrusted clients can start workflows on the queue.
- **A malformed/hostile seed must not wedge the run (DoS).** Because the SDK converts an
  activity **panic** into a *retryable* failure and `render_instructions` carries no retry
  policy (Temporal server-default = unlimited retries), a factory that panics on a bad
  seed would retry-loop forever and hang a workflow with no `RunConfig.timeout`. Mitigations
  (D7): the fallible `try_with_seeded_ctx` maps a seed error to a **non-retryable**
  failure (fail fast); the infallible `with_seeded_ctx` carries a **totality contract**
  (never panic). **For authorization-bearing seeds, prefer `try_with_seeded_ctx`** so a
  bad seed fails the run *loud* rather than silently defaulting to the wrong identity.
- **Per-run policy only runs in modes that reach it.** `authorize_tool` short-circuits
  before the policy under `Bypass` (Allow) and `DontAsk` (Deny). The §4.6 seed-driven
  per-tenant policy is therefore only consulted under `Default`/`AcceptEdits`/`Plan`; a
  worker that also sets `Bypass`/`DontAsk` posture makes its own policy dead code for
  those calls. Documented so operators don't combine them by accident.
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
- `worker::CtxSeedError` (the error type wrapping a `try_with_seeded_ctx` failure).
- `TemporalAgentWorkerBuilder::posture(WorkerPosture<Ctx>)`.
- `TemporalAgentWorkerBuilder::with_seeded_ctx(Fn(Option<Value>) -> Ctx)`.
- `TemporalAgentWorkerBuilder::try_with_seeded_ctx(Fn(Option<Value>) -> Result<Ctx, E>)`.
- `TemporalAgentWorkerBuilder::heartbeat_interval(Duration)`.
- `runner::TemporalRunnerConfig::with_ctx_seed(Value)` (the field itself stays **private**).
- `payloads::WorkflowInput::ctx_seed` (public field, `#[serde(default)]`).

**No new re-exports.** runtime-temporal currently re-exports none of core's posture types
(`lib.rs` has only `pub mod`s), and worker setup already imports `paigasus_helikon_core`
directly (the `lib.rs` doctests do). Users build a `WorkerPosture` by importing
`PermissionMode` / `DenyRule` / `AllowRule` / `GuardRule` / `PermissionPolicy` /
`ApprovalHandler` from `paigasus_helikon_core` (a public dependency). Adding six re-exports
would only add six `///`-doc-comment obligations for no dependency-removal benefit — skip
them. Every genuinely-new `pub` item above carries a `///` doc comment (doc-coverage gate).

## 7. Testing strategy (TDD)

Unit (no Temporal server, fast):
- **Full default-equivalence (drift guard, MAJOR 3):** `WorkerPosture::default().apply(
  RunContext::ephemeral(()))` matches a bare `RunContext::ephemeral(())` on **every**
  posture field — `permission_mode`, `default_guards`, `redact_output`, `deny_rules`,
  `allow_rules`, `guard_rules`, `extra_secrets`, and `permission_policy`/`approval_handler`
  both `None`. Enumerating all nine is what catches a future core knob the bundle forgot.
- `WorkerPosture::apply` installs each knob: a deny rule denies; `Plan` mode blocks a
  write; `without_output_redaction` clears the flag; `without_default_guards` clears it;
  `extra_secrets` accumulate; an approval handler + a guard `Ask` resolves to Allow.
- `invoke_tool_inner` through a `TypedRuntime` with a posture denies a denied tool
  (outcome carries the denial string).
- Seeded factory: `with_seeded_ctx` receives `Some(seed)`; `with_ctx` ignores the seed;
  a `None` seed (no `with_ctx_seed`) reaches the factory as `None`.
- **Malformed-seed fail-fast (BLOCKER, D7):** a `try_with_seeded_ctx` factory that returns
  `Err` makes `run_context` yield a **non-retryable** `ActivityError` (assert
  `is_non_retryable()`), not a panic or a retryable failure. An infallible factory that
  *panics* is out of contract — cover the sanctioned fallible path instead.
- Request-scoped policy: a `PermissionPolicy` reading `ctx.user_ctx()` allows/denies by
  seed content (the §4.6 composition), including that `Bypass` short-circuits the policy
  (MINOR-3 caveat).
- `WorkflowInput` round-trips with and without `ctx_seed`; a legacy JSON object lacking
  the field deserializes (`#[serde(default)]`).
- `build_activity_config` sets `heartbeat_timeout` on model/tool opts when interval is
  `Some`, and leaves it unset when `None`; `render_instructions` never gets one;
  `heartbeat_interval` below the 1 s floor is clamped.
- **Heartbeat ticker preserves the no-leak guarantee:** the rewritten
  `race_with_activity_cancellation` still awaits `work` to completion after cancellation
  (assert the work future is driven to done, not dropped) — the original's load-bearing
  property (MAJOR 2).

Live (env-gated `temporal_live.rs`, loud-skip without a server — mirrors SMA-332):
- A run with a configured seed + a request-scoped policy denies/allows the expected tool.
- (Best-effort / documented) heartbeat reclamation: a killed worker mid-`invoke_tool`
  is re-dispatched faster with a short `heartbeat_interval` than without. If not
  reliably automatable, cover the wiring by unit-asserting `heartbeat_timeout` and note
  the manual validation step in the runbook.

## 8. Docs to update (same PR)

- `runtime-temporal/src/lib.rs` — rewrite "Worker-Side Posture and Security Boundary":
  from "fixed defaults / future work" to "configurable via `WorkerPosture`; defaults
  unchanged; seed mechanism; heartbeats". Add the seed to the payload-budget note (it's
  serialized per `render_instructions`/`invoke_tool` call — keep it small). In the
  heartbeat docs, recommend tools offload blocking work via `tokio::task::spawn_blocking`
  and cross-reference the existing "tool idempotency under crash-retry" warning (§4.5
  caveat). In the determinism/upgrade section, describe the wire additions honestly per
  the revised D6 (additive `#[serde(default)]` field; validate the re-schedule case;
  drain-before-upgrade remains the safe path) — **not** an unqualified "replay-breaking".
- `runtime-temporal/README.md` — posture + seed usage snippet; note published-surface
  changes.
- `docs/superpowers/specs/2026-07-05-runtime-temporal-agentcore-design.md` §5.8 as-built
  note — mark the worker-side configuration and Ctx-seed as **landed in SMA-455** (update
  the two lines that call them future work).
- `docs/book/src/concepts/runtimes.md` — if it documents the fixed-defaults posture,
  bring it in line.
- `CHANGELOG.md` (runtime-temporal) — the new features + an honest upgrade note per the
  revised D6 (additive wire field; validate the activity-reschedule case on upgrade;
  drain-before-upgrade remains the conservative path). Flag as replay-affecting **only**
  if the live determinism check confirms it.

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
