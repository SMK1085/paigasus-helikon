# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-actix-v0.2.11...paigasus-helikon-runtime-actix-v0.3.0) - 2026-11-21

### Breaking changes

- This version replaces `0.2.11`. Version `0.2.11` requires `paigasus-helikon-core` 0.7, but it was released as a patch, so `cargo update` gave it to users who stay on core 0.6. Version `0.2.11` is yanked. The content of this version is the same as `0.2.11`.
- This version requires `paigasus-helikon-core` 0.7. If you stay on core 0.6, use `0.2.10`, the last version for core 0.6. A requirement cannot select a version below its minimum, so if your requirement is higher than `0.2.10` (for example a requirement that `cargo add` wrote for a yanked version), change it to `"0.2.10"` first, then run `cargo update`.
- If you already use core 0.7, change your requirement on `paigasus-helikon-runtime-actix` to `"0.3"`. If you do not, and your requirement allows the core 0.6 versions (for example `"0.2"`), `cargo update` moves you back to the core 0.6 build after the yank, and your build has two copies of core.

## [0.2.11](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-actix-v0.2.10...paigasus-helikon-runtime-actix-v0.2.11) - 2026-11-20

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.2.10](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-actix-v0.2.9...paigasus-helikon-runtime-actix-v0.2.10) - 2026-11-20

### Added

- *(core)* SMA-804 add a turn-boundary checkpoint to runcontext ([#346](https://github.com/SMK1085/paigasus-helikon/pull/346))
- *(core)* SMA-803 add deferred tool calls ([#344](https://github.com/SMK1085/paigasus-helikon/pull/344))

## [0.2.9](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-actix-v0.2.8...paigasus-helikon-runtime-actix-v0.2.9) - 2026-11-12

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.2.8](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-actix-v0.2.7...paigasus-helikon-runtime-actix-v0.2.8) - 2026-10-01

### Other

- update Cargo.toml dependencies

## [0.2.7](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-actix-v0.2.6...paigasus-helikon-runtime-actix-v0.2.7) - 2026-09-06

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.2.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-actix-v0.2.5...paigasus-helikon-runtime-actix-v0.2.6) - 2026-08-23

### Other

- *(workflows)* SMA-581 gate the published markdown surfaces with markdownlint-cli2 ([#220](https://github.com/SMK1085/paigasus-helikon/pull/220))

## [0.2.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-actix-v0.2.4...paigasus-helikon-runtime-actix-v0.2.5) - 2026-08-22

### Added

- *(core)* SMA-568 adopt paigasus:: tracing targets in core and the runtimes ([#216](https://github.com/SMK1085/paigasus-helikon/pull/216))

## [0.2.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-actix-v0.2.3...paigasus-helikon-runtime-actix-v0.2.4) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-runtime-tokio

## [0.2.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-actix-v0.2.2...paigasus-helikon-runtime-actix-v0.2.3) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.2.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-actix-v0.2.1...paigasus-helikon-runtime-actix-v0.2.2) - 2026-08-16

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.2.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-actix-v0.2.0...paigasus-helikon-runtime-actix-v0.2.1) - 2026-08-14

### Added

- *(core)* SMA-422 hoist the terminal-vs-cancel precedence resolver into core ([#193](https://github.com/SMK1085/paigasus-helikon/pull/193))

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-actix-v0.1.0...paigasus-helikon-runtime-actix-v0.2.0) - 2026-08-07

### Added

- *(runtime)* [**breaking**] SMA-482 harden 5xx redaction, session-principal binding, and run caps ([#177](https://github.com/SMK1085/paigasus-helikon/pull/177))
