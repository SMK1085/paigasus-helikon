# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-gemini-v0.2.0...paigasus-helikon-providers-gemini-v0.3.0) - 2026-11-21

### Added

- *(core)* [**breaking**] SMA-830 let a tool declare a provider tool type ([#374](https://github.com/SMK1085/paigasus-helikon/pull/374))

### Fixed

- *(providers)* SMA-830 build the gemini live test tool def with the constructor ([#377](https://github.com/SMK1085/paigasus-helikon/pull/377))
- *(providers)* SMA-827 round trip gemini thought signatures through a provider data part ([#373](https://github.com/SMK1085/paigasus-helikon/pull/373))

### Upgrade notes

- Requires `paigasus-helikon-core` 0.8 (`ToolDef` is now `#[non_exhaustive]`; see the core CHANGELOG).

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-gemini-v0.1.11...paigasus-helikon-providers-gemini-v0.2.0) - 2026-11-21

### Breaking changes

- This version replaces `0.1.11`. Version `0.1.11` requires `paigasus-helikon-core` 0.7, but it was released as a patch, so `cargo update` gave it to users who stay on core 0.6. Version `0.1.11` is yanked. The content of this version is the same as `0.1.11`.
- This version requires `paigasus-helikon-core` 0.7. If you stay on core 0.6, use `0.1.10`, the last version for core 0.6. A requirement cannot select a version below its minimum, so if your requirement is higher than `0.1.10` (for example a requirement that `cargo add` wrote for a yanked version), change it to `"0.1.10"` first, then run `cargo update`.
- If you already use core 0.7, change your requirement on `paigasus-helikon-providers-gemini` to `"0.2"`. If you do not, and your requirement allows the core 0.6 versions (for example `"0.1"`), `cargo update` moves you back to the core 0.6 build after the yank, and your build has two copies of core.

## [0.1.11](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-gemini-v0.1.10...paigasus-helikon-providers-gemini-v0.1.11) - 2026-11-20

### Fixed

- *(providers)* SMA-820 name gemini tool results from their own call and reject a result before its call ([#364](https://github.com/SMK1085/paigasus-helikon/pull/364))

## [0.1.10](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-gemini-v0.1.9...paigasus-helikon-providers-gemini-v0.1.10) - 2026-11-20

### Added

- *(core)* SMA-804 add a turn-boundary checkpoint to runcontext ([#346](https://github.com/SMK1085/paigasus-helikon/pull/346))

## [0.1.9](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-gemini-v0.1.8...paigasus-helikon-providers-gemini-v0.1.9) - 2026-11-12

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.8](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-gemini-v0.1.7...paigasus-helikon-providers-gemini-v0.1.8) - 2026-09-06

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.7](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-gemini-v0.1.6...paigasus-helikon-providers-gemini-v0.1.7) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-gemini-v0.1.5...paigasus-helikon-providers-gemini-v0.1.6) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-gemini-v0.1.4...paigasus-helikon-providers-gemini-v0.1.5) - 2026-08-16

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-gemini-v0.1.3...paigasus-helikon-providers-gemini-v0.1.4) - 2026-08-14

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-gemini-v0.1.2...paigasus-helikon-providers-gemini-v0.1.3) - 2026-07-18

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-gemini-v0.1.1...paigasus-helikon-providers-gemini-v0.1.2) - 2026-07-07

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-gemini-v0.1.0...paigasus-helikon-providers-gemini-v0.1.1) - 2026-06-29

### Other

- updated the following local packages: paigasus-helikon-core
