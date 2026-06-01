# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-v0.2.2...paigasus-helikon-v0.2.3) - 2026-06-01

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-macros, paigasus-helikon-providers-anthropic, paigasus-helikon-providers-openai, paigasus-helikon-runtime-tokio, paigasus-helikon-sessions-sqlite

## [0.2.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-v0.2.1...paigasus-helikon-v0.2.2) - 2026-05-31

### Added

- *(core)* SMA-322 emit opentelemetry genai-semconv spans ([#51](https://github.com/SMK1085/paigasus-helikon/pull/51))

## [0.2.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-v0.2.0...paigasus-helikon-v0.2.1) - 2026-05-30

### Other

- Re-release to refresh feature-gated dependency requirements. The facade now requires `paigasus-helikon-core` `^0.2.3` and `paigasus-helikon-runtime-tokio` `^0.1.1`, so the SMA-346 structured-`AgentError`-at-the-runner-boundary surface is reachable through the facade's `runtime-tokio` feature. No facade source changes; the prior 0.2.0 publish predated the runtime-tokio `0.0.0`→`0.1.x` ascent and still requested the `^0.0.0` stub.

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-v0.1.1...paigasus-helikon-v0.2.0) - 2026-05-29

### Added

- *(core)* [**breaking**] SMA-320 honor output_type<T> with structured validation and one-shot repair ([#43](https://github.com/SMK1085/paigasus-helikon/pull/43))

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-v0.1.0...paigasus-helikon-v0.1.1) - 2026-05-28

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-macros, paigasus-helikon-providers-anthropic, paigasus-helikon-providers-openai, paigasus-helikon-sessions-sqlite

## [0.1.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-v0.1.0) - 2026-05-22

### Added

- *(facade)* SMA-304 add paigasus-helikon facade with feature-gated re-exports

### Other

- *(release)* SMA-347 escape release-plz 0.0.0 trap for core and facade ([#22](https://github.com/SMK1085/paigasus-helikon/pull/22))
- *(workspace)* SMA-335 enforce Conventional Commits (CI + PR title + local hook) ([#12](https://github.com/SMK1085/paigasus-helikon/pull/12))
- release v0.0.0 ([#6](https://github.com/SMK1085/paigasus-helikon/pull/6))
- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))
- *(readme)* SMA-304 clarify pre-release version in install snippet

## [0.0.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-v0.0.0) - 2026-05-17

### Added

- *(facade)* SMA-304 add paigasus-helikon facade with feature-gated re-exports

### Other

- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))
- *(readme)* SMA-304 clarify pre-release version in install snippet
