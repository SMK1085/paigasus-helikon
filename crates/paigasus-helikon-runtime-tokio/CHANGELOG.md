# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.14](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.13...paigasus-helikon-runtime-tokio-v0.1.14) - 2026-06-20

### Added

- *(core)* SMA-403 add runcontext ephemeral constructor and dependency setters ([#105](https://github.com/SMK1085/paigasus-helikon/pull/105))

## [0.1.13](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.12...paigasus-helikon-runtime-tokio-v0.1.13) - 2026-06-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.12](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.11...paigasus-helikon-runtime-tokio-v0.1.12) - 2026-06-18

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.11](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.10...paigasus-helikon-runtime-tokio-v0.1.11) - 2026-06-17

### Added

- *(runtime-tokio)* SMA-393 add RetryingModel retry decorator for transient errors ([#97](https://github.com/SMK1085/paigasus-helikon/pull/97))

## [0.1.10](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.9...paigasus-helikon-runtime-tokio-v0.1.10) - 2026-06-16

### Other

- *(repo)* SMA-424 refresh crate READMEs to match the shipped surface ([#93](https://github.com/SMK1085/paigasus-helikon/pull/93))

## [0.1.9](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.8...paigasus-helikon-runtime-tokio-v0.1.9) - 2026-06-16

### Fixed

- *(runtime-tokio)* SMA-421 keep a genuine terminal over a late cancel/timeout ([#86](https://github.com/SMK1085/paigasus-helikon/pull/86))

## [0.1.8](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.7...paigasus-helikon-runtime-tokio-v0.1.8) - 2026-06-15

### Added

- *(runtime-tokio)* SMA-392 wire session persistence into the run lifecycle ([#84](https://github.com/SMK1085/paigasus-helikon/pull/84))

## [0.1.7](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.6...paigasus-helikon-runtime-tokio-v0.1.7) - 2026-06-14

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.5...paigasus-helikon-runtime-tokio-v0.1.6) - 2026-06-08

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.4...paigasus-helikon-runtime-tokio-v0.1.5) - 2026-06-04

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.3...paigasus-helikon-runtime-tokio-v0.1.4) - 2026-06-04

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.2...paigasus-helikon-runtime-tokio-v0.1.3) - 2026-06-01

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.1...paigasus-helikon-runtime-tokio-v0.1.2) - 2026-05-31

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.0...paigasus-helikon-runtime-tokio-v0.1.1) - 2026-05-30

### Added

- *(runtime-tokio)* SMA-346 surface the structured `AgentError` at the runner boundary. `TokioRunner::run` now returns `RunError::Agent(AgentError::…)` (e.g. `MaxTurnsExceeded`, `Model`) for agent failures instead of an opaque `RunError::Other(String)`, by wiring the `RunContext` failure slot through `RunResultStreaming::with_failure`. `run_streamed` carries the slot too. Cancellation/timeout remain `RunError::Cancelled`/`Timeout` (runner-level, not slot-backed).

## [0.0.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-runtime-tokio-v0.0.0) - 2026-05-17

### Added

- *(runtime)* SMA-304 add tokio, axum, temporal, agentcore runtime stubs

### Other

- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))
