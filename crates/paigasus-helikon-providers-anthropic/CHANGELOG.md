# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.30...paigasus-helikon-providers-anthropic-v0.2.0) - 2026-11-21

### Breaking changes

- This version replaces `0.1.29` and `0.1.30`. These versions require `paigasus-helikon-core` 0.7, but they were released as patches, so `cargo update` gave them to users who stay on core 0.6. Both versions are yanked. The content of this version is the same as `0.1.30`.
- This version requires `paigasus-helikon-core` 0.7. If you stay on core 0.6, use `0.1.28`, the last version for core 0.6. A requirement cannot select a version below its minimum, so if your requirement is higher than `0.1.28` (for example a requirement that `cargo add` wrote for a yanked version), change it to `"0.1.28"` first, then run `cargo update`.
- If you already use core 0.7, change your requirement on `paigasus-helikon-providers-anthropic` to `"0.2"`. If you do not, and your requirement allows the core 0.6 versions (for example `"0.1"`), `cargo update` moves you back to the core 0.6 build after the yank, and your build has two copies of core.

## [0.1.30](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.29...paigasus-helikon-providers-anthropic-v0.1.30) - 2026-11-20

### Fixed

- *(providers)* SMA-834 let tool-use-only assistant messages join an open anthropic round ([#368](https://github.com/SMK1085/paigasus-helikon/pull/368))

## [0.1.29](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.28...paigasus-helikon-providers-anthropic-v0.1.29) - 2026-11-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.28](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.27...paigasus-helikon-providers-anthropic-v0.1.28) - 2026-11-20

### Fixed

- *(providers-anthropic)* SMA-819 drop empty text blocks and keep tool rounds in order ([#353](https://github.com/SMK1085/paigasus-helikon/pull/353))

## [0.1.27](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.26...paigasus-helikon-providers-anthropic-v0.1.27) - 2026-11-20

### Added

- *(core)* SMA-804 add a turn-boundary checkpoint to runcontext ([#346](https://github.com/SMK1085/paigasus-helikon/pull/346))

## [0.1.26](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.25...paigasus-helikon-providers-anthropic-v0.1.26) - 2026-11-12

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.25](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.24...paigasus-helikon-providers-anthropic-v0.1.25) - 2026-09-06

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.24](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.23...paigasus-helikon-providers-anthropic-v0.1.24) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.23](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.22...paigasus-helikon-providers-anthropic-v0.1.23) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.22](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.21...paigasus-helikon-providers-anthropic-v0.1.22) - 2026-08-17

### Fixed

- *(providers-anthropic)* SMA-531 emit finish when a stream body ends before message_stop ([#200](https://github.com/SMK1085/paigasus-helikon/pull/200))

## [0.1.21](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.20...paigasus-helikon-providers-anthropic-v0.1.21) - 2026-08-16

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.20](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.19...paigasus-helikon-providers-anthropic-v0.1.20) - 2026-08-14

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.19](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.18...paigasus-helikon-providers-anthropic-v0.1.19) - 2026-07-18

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.18](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.17...paigasus-helikon-providers-anthropic-v0.1.18) - 2026-07-07

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.17](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.16...paigasus-helikon-providers-anthropic-v0.1.17) - 2026-06-29

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.16](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.15...paigasus-helikon-providers-anthropic-v0.1.16) - 2026-06-21

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.15](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.14...paigasus-helikon-providers-anthropic-v0.1.15) - 2026-06-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.14](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.13...paigasus-helikon-providers-anthropic-v0.1.14) - 2026-06-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.13](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.12...paigasus-helikon-providers-anthropic-v0.1.13) - 2026-06-18

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.12](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.11...paigasus-helikon-providers-anthropic-v0.1.12) - 2026-06-17

### Added

- *(runtime-tokio)* SMA-393 add RetryingModel retry decorator for transient errors ([#97](https://github.com/SMK1085/paigasus-helikon/pull/97))

## [0.1.11](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.10...paigasus-helikon-providers-anthropic-v0.1.11) - 2026-06-16

### Other

- *(repo)* SMA-424 refresh crate READMEs to match the shipped surface ([#93](https://github.com/SMK1085/paigasus-helikon/pull/93))

## [0.1.10](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.9...paigasus-helikon-providers-anthropic-v0.1.10) - 2026-06-16

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.9](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.8...paigasus-helikon-providers-anthropic-v0.1.9) - 2026-06-15

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.8](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.7...paigasus-helikon-providers-anthropic-v0.1.8) - 2026-06-14

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.7](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.6...paigasus-helikon-providers-anthropic-v0.1.7) - 2026-06-08

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.5...paigasus-helikon-providers-anthropic-v0.1.6) - 2026-06-04

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.4...paigasus-helikon-providers-anthropic-v0.1.5) - 2026-06-04

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.3...paigasus-helikon-providers-anthropic-v0.1.4) - 2026-06-01

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.2...paigasus-helikon-providers-anthropic-v0.1.3) - 2026-05-31

### Added

- *(core)* SMA-322 emit opentelemetry genai-semconv spans ([#51](https://github.com/SMK1085/paigasus-helikon/pull/51))

## [0.1.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.1...paigasus-helikon-providers-anthropic-v0.1.2) - 2026-05-29

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.0...paigasus-helikon-providers-anthropic-v0.1.1) - 2026-05-28

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-providers-anthropic-v0.1.0) - 2026-05-26

### Added

- *(providers-anthropic)* SMA-317 add Anthropic provider with streaming, tools, caching ([#33](https://github.com/SMK1085/paigasus-helikon/pull/33))
- *(providers)* SMA-304 add openai and anthropic provider stubs

### Other

- release v0.0.0 ([#6](https://github.com/SMK1085/paigasus-helikon/pull/6))
- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))

## [0.0.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-providers-anthropic-v0.0.0) - 2026-05-17

### Added

- *(providers)* SMA-304 add openai and anthropic provider stubs

### Other

- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))
