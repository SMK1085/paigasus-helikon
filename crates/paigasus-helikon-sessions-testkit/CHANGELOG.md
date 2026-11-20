# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Upgrade notes

- Requires `paigasus-helikon-core` 0.8 (`ToolDef` is now `#[non_exhaustive]`; see the core CHANGELOG).

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-testkit-v0.1.3...paigasus-helikon-sessions-testkit-v0.2.0) - 2026-11-21

### Breaking changes

- This version replaces `0.1.3`. Version `0.1.3` requires `paigasus-helikon-core` 0.7, but it was released as a patch, so `cargo update` gave it to users who stay on core 0.6. Version `0.1.3` is yanked. The content of this version is the same as `0.1.3`.
- This version requires `paigasus-helikon-core` 0.7. If you stay on core 0.6, use `0.1.2`, the last version for core 0.6. A requirement cannot select a version below its minimum, so if your requirement is higher than `0.1.2` (for example a requirement that `cargo add` wrote for a yanked version), change it to `"0.1.2"` first, then run `cargo update`.
- If you already use core 0.7, change your requirement on `paigasus-helikon-sessions-testkit` to `"0.2"`. If you do not, and your requirement allows the core 0.6 versions (for example `"0.1"`), `cargo update` moves you back to the core 0.6 build after the yank, and your build has two copies of core.

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-testkit-v0.1.2...paigasus-helikon-sessions-testkit-v0.1.3) - 2026-11-20

### Other

- update Cargo.toml dependencies

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
