# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-testkit-v0.1.2...paigasus-helikon-sessions-testkit-v0.1.3) - 2026-11-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-testkit-v0.1.1...paigasus-helikon-sessions-testkit-v0.1.2) - 2026-11-20

### Added

- *(sessions)* SMA-816 add a ToolDeferred conformance sub-test to the testkit ([#352](https://github.com/SMK1085/paigasus-helikon/pull/352))

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-testkit-v0.1.0...paigasus-helikon-sessions-testkit-v0.1.1) - 2026-11-20

### Other

- update Cargo.toml dependencies

## [0.1.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-sessions-testkit-v0.1.0) - 2026-11-19

### Added

- *(sessions)* SMA-805 first crates.io release of the shared `Session` conformance suite
  - `run_all` runs every conformance test against a factory that returns a fresh, empty `Arc<dyn Session>`
  - `run_append_read`, `run_watermark_exclusive`, `run_projection` and `run_concurrent_writers` are public, so a backend can run a subset
