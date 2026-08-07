# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-temporal-v0.3.0...paigasus-helikon-runtime-temporal-v0.3.1) - 2026-08-07

### Other

- *(workflows)* SMA-457 add temporal-it and agentcore-image integration jobs ([#181](https://github.com/SMK1085/paigasus-helikon/pull/181))

## [0.3.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-temporal-v0.2.2...paigasus-helikon-runtime-temporal-v0.3.0) - 2026-08-07

### Added

- *(runtime-temporal)* [**breaking**] SMA-484 remove the pre-envelope activity-input decode arms ([#180](https://github.com/SMK1085/paigasus-helikon/pull/180))

### Changed

- *(runtime-temporal)* SMA-484 activity inputs are **envelope-only**
  - The pre-envelope positional decode arms are removed. A worker on this version that **is** handed one of those payloads logs at `ERROR` and fails the attempt with a decode error naming the activity, the payload count, and the recovery version, rather than executing it.
  - No public Rust API change — the envelope types are crate-internal. The break is on the wire only.

### Upgrade notes

- **Upgrading from 0.2.0 or 0.2.1 requires a stop at 0.2.2.** That release decodes both the envelope and those two releases' pre-envelope positional shapes, so it is the migration bridge: land the fleet on 0.2.2, **drain in-flight runs while it is there**, then take this version. *Drain* means every workflow execution on the task queue has reached a terminal state — not merely that new runs have been paused.
- **0.1.x cannot use the bridge.** 0.2.2 decodes the 0.2.0/0.2.1 arities specifically; only `call_model`'s 2-payload shape happens to be unchanged since 0.1.x. A 0.1.x `render_instructions` task (1 payload) and `invoke_tool` task (2 payloads) both fail closed on 0.2.2 as well, so a 0.1.x fleet must drain or terminate its in-flight runs outright rather than hopping through 0.2.2. The decode error's own wording says "0.2.1 and earlier" because it describes the shape that arrived, not the supported upgrade path.
- **0.2.2 ↔ this version is compatible in both directions for activity inputs.** Both encode and decode the envelope, so a rolling 0.2.2 → 0.3.0 upgrade needs no drain on account of this change. (Scope: activity inputs. `WorkflowInput` and activity outputs are unchanged here and carry their own compatibility story.)
- **If a worker on this version meets a pre-envelope task anyway**, the attempt fails *retryably* and Temporal re-dispatches it. Any 0.2.2 worker still polling the queue will decode and execute it — so the recovery is to re-join one, let in-flight runs drain, then remove it. For a run that cannot be drained in an acceptable window, use a blue-green task queue or terminate the execution.
- **Do not expect the failure to self-terminate.** `render_instructions` is built with no retry policy, so the server default of unlimited attempts applies, and `WorkflowInput::timeout_ms` is `None` unless set. On a default configuration the retry loop is unbounded and recovery is operator action. The opposite hazard also applies: a **finite** `maximum_attempts` on `model_retry_policy` / `tool_retry_policy` does bound the retry, so on a `call_model` / `invoke_tool` rejection the window to re-join a 0.2.2 worker can be seconds — see [docs.rs, § "Upgrade Discipline and Determinism"](https://docs.rs/paigasus-helikon-runtime-temporal) for all four bounds.
- **Rolling back below 0.2.2 still requires a drain**, unchanged: a queued envelope payload is frozen in history and re-delivered on every retry. Rolling back to 0.2.2 is safe.

## [0.2.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-temporal-v0.2.1...paigasus-helikon-runtime-temporal-v0.2.2) - 2026-08-07

### Added

- *(runtime-temporal)* SMA-462 replace positional activity inputs with a versionable envelope ([#176](https://github.com/SMK1085/paigasus-helikon/pull/176))

### Changed

- *(runtime-temporal)* SMA-462 activity inputs are now a **single self-describing envelope payload**
  - `render_instructions`, `call_model` and `invoke_tool` each take one JSON-object payload instead of positional arguments. Workers on this version also decode the previous pre-envelope positional shapes from 0.2.0 or 0.2.1 specifically, so activity tasks queued by a worker on either of those two versions execute normally. 0.1.x is outside the support window: its `render_instructions` and `invoke_tool` shapes fail closed with a decode error rather than being silently misread.
  - No public Rust API change — the envelope types are crate-internal.
  - Future additive changes to an activity's input are now compatible in both directions (unknown fields ignored, absent fields defaulted). This is scoped to the envelope's own field set: it does **not** cover the `paigasus-helikon-core` types nested inside them (`ModelRequest`, `ToolCallRequest`) or activity outputs.

### Upgrade notes

- **Upgrade one release at a time**, and keep the mixed-fleet window short. A 0.2.1-and-earlier worker handed one of the new envelope payloads cannot decode it; it fails the attempt retryably and Temporal re-dispatches until a worker on this version takes it. Four things bound that recovery: a finite `maximum_attempts` on `model_retry_policy` / `tool_retry_policy` can be exhausted; `WorkflowInput::timeout_ms` interrupts the run regardless of retry policy; a terminal `render_instructions` failure ends the run outright; and exhausted `invoke_tool` retries are folded into a tool-error result fed to the model rather than failing loudly.
- **Prefer draining in-flight runs before this upgrade**, or ensure retry caps are unlimited and run deadlines generous for the duration of the rollout. Blue-green task queues remain available.
- **Rolling back requires a drain.** Once a worker on this version has queued an envelope-shaped activity task, that payload is frozen in the `ActivityTaskScheduled` event and every retry re-delivers it; a rollback to 0.2.1 and earlier leaves those activities undecodable until the run deadline.
- Activity input encoding is **not** a replay hazard: Temporal's replay check compares an activity's id and type only, never its input payloads. Verified against `temporalio-* = 0.5.0`; re-verify on any SDK bump.

## [0.2.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-temporal-v0.2.0...paigasus-helikon-runtime-temporal-v0.2.1) - 2026-07-18

### Other

- updated the following local packages: paigasus-helikon-core

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-temporal-v0.1.1...paigasus-helikon-runtime-temporal-v0.2.0) - 2026-07-09

### Added

- *(runtime-temporal)* SMA-455 add worker-side posture, ctx seed, and heartbeats ([#139](https://github.com/SMK1085/paigasus-helikon/pull/139))

### Added

- *(runtime-temporal)* SMA-455 worker-side posture configuration, request-scoped `Ctx` seed, and opt-in heartbeats
  - `worker::WorkerPosture<Ctx>` — a grouped builder for the nine posture knobs `RunContext` already exposes (permission mode, deny/allow/guard rules, permission policy, approval handler, default guards, output redaction, extra secrets), set via `TemporalAgentWorkerBuilder::posture(...)`; `WorkerPosture::default()` reproduces the v0 fixed defaults exactly, so a worker that never calls `.posture(...)` behaves byte-for-byte as before
  - `TemporalAgentWorkerBuilder::with_seeded_ctx` / `::try_with_seeded_ctx` — seeded `Ctx` factories that reconstitute the per-run context from a client-supplied `serde_json::Value` seed; `try_with_seeded_ctx` maps a rejected seed to a **non-retryable** activity failure (fail-fast on a malformed/hostile seed) rather than a silent default identity
  - `runner::TemporalRunnerConfig::with_ctx_seed` — attaches the request-scoped seed on the client side; threaded through `payloads::WorkflowInput::ctx_seed` (new, `#[serde(default)]`, public field)
  - `TemporalAgentWorkerBuilder::heartbeat_interval` — opt-in liveness heartbeats on the `call_model`/`invoke_tool` activities (floored to 1s; sets `heartbeat_timeout = 2 × interval` on those two activities only), speeding up crash reclamation; off by default
  - No new re-exports — build a `WorkerPosture` by importing `PermissionMode` / `DenyRule` / `AllowRule` / `GuardRule` / `PermissionPolicy` / `ApprovalHandler` from `paigasus-helikon-core` directly

### Upgrade notes

- The wire additions above are **additive by construction**: `WorkflowInput::ctx_seed` is `#[serde(default)]`, so a payload serialized by a pre-SMA-455 worker still deserializes; the changed `render_instructions`/`invoke_tool` activity-input shape only affects **newly-scheduled** activities (an already-completed activity replays its recorded result from history, it does not re-execute). This is a reasoned claim, not a guarantee proven against every upgrade path — **drain-before-upgrade / blue-green task queues remain the conservative, recommended path** for any worker-version bump, same as before this release.
- Manually validated via the crate's unit test suite (seed round-trip, default-equivalence, fail-fast-on-bad-seed, heartbeat wiring); no automated live-Temporal determinism/replay check for the new activity-input shape ships in this release.

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-temporal-v0.1.0...paigasus-helikon-runtime-temporal-v0.1.1) - 2026-07-07

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-runtime-temporal-v0.1.0) - 2026-07-06

### Added

- *(runtime-temporal)* SMA-332 initial real implementation: Temporal-backed durable `Runner`
  - `TemporalRunner` implementing `paigasus-helikon-core`'s `Runner` trait — agent runs execute as a deterministic Temporal workflow, replayed from history on resumption, resuming from the last completed activity after a worker crash
  - `TemporalAgentWorker` builder — registers `LlmAgent`s onto a task queue, serving the agent-loop workflow and its activities
  - Pure durable loop driver decoupling agent-loop decisions from Temporal's workflow-context APIs, enabling deterministic replay
  - Per-activity timeout overrides on the worker builder
  - Registration fails fast (`RegistrationError`) on hooks, guardrails, and handoffs — not supported by the v0 constraint set; nested agent-as-tool is permitted but runs non-durably inside its tool activity
  - Worker-side posture: the worker fabricates its own `RunContext` rather than accepting one from the client, and applies fixed safe defaults (redaction on, destructive-effect guards on); worker-side posture configuration is future work
  - Env-gated live integration suite (`TEMPORAL_TEST_SERVER`) covering crash-resume, cancellation with partial transcripts, session persistence, and model error handling

## [0.0.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-runtime-temporal-v0.0.0) - 2026-05-17

### Added

- *(runtime)* SMA-304 add tokio, axum, temporal, agentcore runtime stubs

### Other

- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))
