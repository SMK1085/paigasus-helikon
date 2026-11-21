# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.2.0...paigasus-helikon-runtime-tokio-v0.3.0) - 2026-11-21

### Added

- *(core)* [**breaking**] SMA-830 let a tool declare a provider tool type ([#374](https://github.com/SMK1085/paigasus-helikon/pull/374))

### Upgrade notes

- Requires `paigasus-helikon-core` 0.8 (`ToolDef` is now `#[non_exhaustive]`; see the core CHANGELOG).

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.27...paigasus-helikon-runtime-tokio-v0.2.0) - 2026-11-21

### Breaking changes

- This version replaces `0.1.27`. Version `0.1.27` requires `paigasus-helikon-core` 0.7, but it was released as a patch, so `cargo update` gave it to users who stay on core 0.6. Version `0.1.27` is yanked. The content of this version is the same as `0.1.27`.
- This version requires `paigasus-helikon-core` 0.7. If you stay on core 0.6, use `0.1.26`, the last version for core 0.6. A requirement cannot select a version below its minimum, so if your requirement is higher than `0.1.26` (for example a requirement that `cargo add` wrote for a yanked version), change it to `"0.1.26"` first, then run `cargo update`.
- If you already use core 0.7, change your requirement on `paigasus-helikon-runtime-tokio` to `"0.2"`. If you do not, and your requirement allows the core 0.6 versions (for example `"0.1"`), `cargo update` moves you back to the core 0.6 build after the yank, and your build has two copies of core.

## [0.1.27](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.26...paigasus-helikon-runtime-tokio-v0.1.27) - 2026-11-20

### Fixed

- *(core)* SMA-810 give each handoff transfer call a tool result ([#358](https://github.com/SMK1085/paigasus-helikon/pull/358))

### Other

- *(core)* SMA-828 share the deferral test helpers with runtime-tokio ([#366](https://github.com/SMK1085/paigasus-helikon/pull/366))
- *(core)* SMA-826 move the shared core test helpers into tests/common ([#359](https://github.com/SMK1085/paigasus-helikon/pull/359))

## [0.1.26](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.25...paigasus-helikon-runtime-tokio-v0.1.26) - 2026-11-20

### Added

- *(core)* SMA-804 add a turn-boundary checkpoint to runcontext ([#346](https://github.com/SMK1085/paigasus-helikon/pull/346))
- *(core)* SMA-803 add deferred tool calls ([#344](https://github.com/SMK1085/paigasus-helikon/pull/344))

## [0.1.25](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.24...paigasus-helikon-runtime-tokio-v0.1.25) - 2026-11-12

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.24](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.23...paigasus-helikon-runtime-tokio-v0.1.24) - 2026-09-06

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.23](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.22...paigasus-helikon-runtime-tokio-v0.1.23) - 2026-08-22

### Added

- *(core)* SMA-568 adopt paigasus:: tracing targets in core and the runtimes ([#216](https://github.com/SMK1085/paigasus-helikon/pull/216))

## [0.1.22](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.21...paigasus-helikon-runtime-tokio-v0.1.22) - 2026-08-22

### Fixed

- *(evals)* SMA-563 terminate the mockmodel stream on cancellation ([#214](https://github.com/SMK1085/paigasus-helikon/pull/214))

## [0.1.21](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.20...paigasus-helikon-runtime-tokio-v0.1.21) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.20](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.19...paigasus-helikon-runtime-tokio-v0.1.20) - 2026-08-16

### Added

- *(core)* SMA-515 add terminal_event and From<RunInterrupt> for RunError ([#195](https://github.com/SMK1085/paigasus-helikon/pull/195))

## [0.1.19](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.18...paigasus-helikon-runtime-tokio-v0.1.19) - 2026-08-14

### Added

- *(core)* SMA-422 hoist the terminal-vs-cancel precedence resolver into core ([#193](https://github.com/SMK1085/paigasus-helikon/pull/193))

## [0.1.18](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.17...paigasus-helikon-runtime-tokio-v0.1.18) - 2026-07-18

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.17](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.16...paigasus-helikon-runtime-tokio-v0.1.17) - 2026-07-07

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.16](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.15...paigasus-helikon-runtime-tokio-v0.1.16) - 2026-06-29

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.15](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-tokio-v0.1.14...paigasus-helikon-runtime-tokio-v0.1.15) - 2026-06-21

### Other

- updated the following local packages: paigasus-helikon-core

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
