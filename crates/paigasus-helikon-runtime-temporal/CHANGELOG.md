# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
