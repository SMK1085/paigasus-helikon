# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.3.1...paigasus-helikon-evals-v0.3.2) - 2026-11-22

### Fixed

- *(sessions)* SMA-822 stop sessions-postgres from compiling sqlite ([#389](https://github.com/SMK1085/paigasus-helikon/pull/389))

## [0.3.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.3.0...paigasus-helikon-evals-v0.3.1) - 2026-11-21

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.3.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.2.0...paigasus-helikon-evals-v0.3.0) - 2026-11-21

### Added

- *(core)* [**breaking**] SMA-830 let a tool declare a provider tool type ([#374](https://github.com/SMK1085/paigasus-helikon/pull/374))

### Upgrade notes

- Requires `paigasus-helikon-core` 0.8 (`ToolDef` is now `#[non_exhaustive]`; see the core CHANGELOG).

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.22...paigasus-helikon-evals-v0.2.0) - 2026-11-21

### Breaking changes

- This version replaces `0.1.22`. Version `0.1.22` requires `paigasus-helikon-core` 0.7, but it was released as a patch, so `cargo update` gave it to users who stay on core 0.6. Version `0.1.22` is yanked. The content of this version is the same as `0.1.22`.
- This version requires `paigasus-helikon-core` 0.7. If you stay on core 0.6, use `0.1.21`, the last version for core 0.6. A requirement cannot select a version below its minimum, so if your requirement is higher than `0.1.21` (for example a requirement that `cargo add` wrote for a yanked version), change it to `"0.1.21"` first, then run `cargo update`.
- If you already use core 0.7, change your requirement on `paigasus-helikon-evals` to `"0.2"`. If you do not, and your requirement allows the core 0.6 versions (for example `"0.1"`), `cargo update` moves you back to the core 0.6 build after the yank, and your build has two copies of core.

## [0.1.22](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.21...paigasus-helikon-evals-v0.1.22) - 2026-11-20

### Other

- update Cargo.toml dependencies

## [0.1.21](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.20...paigasus-helikon-evals-v0.1.21) - 2026-11-20

### Security

- Require `sqlx` 0.9.1 or later ([#351](https://github.com/SMK1085/paigasus-helikon/pull/351)). This addresses RUSTSEC-2026-0459 (CVE-2026-119375): `libsqlite3-sys` before 0.39 bundles an affected SQLite. `sqlx` 0.9.0 caps `libsqlite3-sys` below 0.38, so it cannot resolve a fixed version.
- The new floor does not update an existing lockfile. Run `cargo update -p libsqlite3-sys`, then check that `cargo tree -i libsqlite3-sys` shows 0.39 or later. If another dependency caps it lower, update that dependency and run the update again. Builds with `bundled-sqlcipher` must also check the bundled SQLCipher version.

### Other

- update Cargo.toml dependencies

## [0.1.20](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.19...paigasus-helikon-evals-v0.1.20) - 2026-11-20

### Other

- update Cargo.toml dependencies

## [0.1.19](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.18...paigasus-helikon-evals-v0.1.19) - 2026-11-16

### Other

- update Cargo.toml dependencies

## [0.1.18](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.17...paigasus-helikon-evals-v0.1.18) - 2026-11-12

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.1.17](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.16...paigasus-helikon-evals-v0.1.17) - 2026-10-30

### Other

- update Cargo.toml dependencies

## [0.1.16](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.15...paigasus-helikon-evals-v0.1.16) - 2026-10-01

### Other

- update Cargo.toml dependencies

## [0.1.15](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.14...paigasus-helikon-evals-v0.1.15) - 2026-09-22

### Other

- update Cargo.toml dependencies

## [0.1.14](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.13...paigasus-helikon-evals-v0.1.14) - 2026-09-17

### Other

- update Cargo.toml dependencies

## [0.1.13](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.12...paigasus-helikon-evals-v0.1.13) - 2026-09-07

### Other

- update Cargo.toml dependencies

## [0.1.12](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.11...paigasus-helikon-evals-v0.1.12) - 2026-09-06

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.1.11](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.10...paigasus-helikon-evals-v0.1.11) - 2026-09-02

### Other

- update Cargo.toml dependencies

## [0.1.10](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.9...paigasus-helikon-evals-v0.1.10) - 2026-08-29

### Other

- update Cargo.toml dependencies

## [0.1.9](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.8...paigasus-helikon-evals-v0.1.9) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.1.8](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.7...paigasus-helikon-evals-v0.1.8) - 2026-08-22

### Fixed

- *(evals)* SMA-563 terminate the mockmodel stream on cancellation ([#214](https://github.com/SMK1085/paigasus-helikon/pull/214))

## [0.1.7](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.6...paigasus-helikon-evals-v0.1.7) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.1.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.5...paigasus-helikon-evals-v0.1.6) - 2026-08-16

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.1.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.4...paigasus-helikon-evals-v0.1.5) - 2026-08-14

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.1.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.3...paigasus-helikon-evals-v0.1.4) - 2026-08-04

### Other

- update Cargo.toml dependencies

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.2...paigasus-helikon-evals-v0.1.3) - 2026-07-18

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.1.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.1...paigasus-helikon-evals-v0.1.2) - 2026-07-13

### Other

- update Cargo.toml dependencies

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-evals-v0.1.0...paigasus-helikon-evals-v0.1.1) - 2026-07-07

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.0.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-evals-v0.0.0) - 2026-05-17

### Added

- *(evals)* SMA-304 add paigasus-helikon-evals stub crate

### Other

- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))
