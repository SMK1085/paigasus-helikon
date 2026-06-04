# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-core-v0.3.0...paigasus-helikon-core-v0.4.0) - 2026-06-04

### Added

- *(core)* [**breaking**] SMA-324 add multi-agent handoff + AgentAsTool ([#61](https://github.com/SMK1085/paigasus-helikon/pull/61))

## [0.3.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-core-v0.2.4...paigasus-helikon-core-v0.3.0) - 2026-06-01

### Fixed

- *(core)* [**breaking**] SMA-402 report cumulative token usage across all turns ([#53](https://github.com/SMK1085/paigasus-helikon/pull/53))

## [0.2.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-core-v0.2.3...paigasus-helikon-core-v0.2.4) - 2026-05-31

### Added

- *(core)* SMA-322 emit opentelemetry genai-semconv spans ([#51](https://github.com/SMK1085/paigasus-helikon/pull/51))

## [0.2.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-core-v0.2.2...paigasus-helikon-core-v0.2.3) - 2026-05-30

### Other

- *(core)* SMA-346 derive `Debug` on `FailureSlot` (public type) and drop a redundant `String` clone at the `build_items` failure site. Published alongside `paigasus-helikon-runtime-tokio` 0.1.1, which wires the runner boundary to this API.

## [0.2.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-core-v0.2.1...paigasus-helikon-core-v0.2.2) - 2026-05-29

### Added

- *(core)* SMA-346 surface the structured `AgentError` at the runner boundary: add `FailureSlot`, `RunContext::failure_handle`, and `RunResultStreaming::with_failure`. `Runner::run` and `collect`/`collect_typed` now return `RunError::Agent(AgentError::…)` for agent failures instead of an opaque string; `AgentEvent::RunFailed { error: String }` is unchanged. Publishes the API that `paigasus-helikon-runtime-tokio` depends on in the same change.

## [0.2.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-core-v0.2.0...paigasus-helikon-core-v0.2.1) - 2026-05-29

### Added

- *(core)* SMA-321 add `RunConfig::timeout` and `parallel_tool_call_limit` (+ builders), `RunError::Timeout`, `RunContext::{with_run_config, run_config}`, and bounded tool-call concurrency. Publishes the API that `paigasus-helikon-runtime-tokio` 0.1.0 depends on (the runtime crate's first publish failed against the stale core 0.2.0).

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-core-v0.1.1...paigasus-helikon-core-v0.2.0) - 2026-05-29

### Added

- *(core)* [**breaking**] SMA-320 honor output_type<T> with structured validation and one-shot repair ([#43](https://github.com/SMK1085/paigasus-helikon/pull/43))

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-core-v0.1.0...paigasus-helikon-core-v0.1.1) - 2026-05-28

### Other

- *(core)* SMA-386 re-bless trybuild stderr for rustc 1.96.0 ([#41](https://github.com/SMK1085/paigasus-helikon/pull/41))

## [0.1.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-core-v0.1.0) - 2026-05-22

### Added

- *(core)* SMA-314 add LlmAgent + explicit LoopState state machine ([#20](https://github.com/SMK1085/paigasus-helikon/pull/20))
- *(core)* SMA-313 concrete shared types (Item, AgentEvent, RunContext, RunResult, ToolContext) ([#18](https://github.com/SMK1085/paigasus-helikon/pull/18))
- *(core)* SMA-312 define core trait surface ([#17](https://github.com/SMK1085/paigasus-helikon/pull/17))
- *(core)* SMA-307 add release-plz smoketest docstring ([#7](https://github.com/SMK1085/paigasus-helikon/pull/7))
- *(core)* SMA-304 add paigasus-helikon-core stub crate

### Other

- *(release)* SMA-347 escape release-plz 0.0.0 trap for core and facade ([#22](https://github.com/SMK1085/paigasus-helikon/pull/22))
- release v0.0.0 ([#6](https://github.com/SMK1085/paigasus-helikon/pull/6))
- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))

## [0.0.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-core-v0.0.0) - 2026-05-17

### Added

- *(core)* SMA-304 add paigasus-helikon-core stub crate

### Other

- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))
