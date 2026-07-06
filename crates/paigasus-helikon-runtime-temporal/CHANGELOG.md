# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
