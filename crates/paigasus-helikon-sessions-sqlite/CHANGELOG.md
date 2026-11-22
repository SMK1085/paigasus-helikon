# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.3.1...paigasus-helikon-sessions-sqlite-v0.3.2) - 2026-11-22

### Fixed

- *(sessions)* SMA-822 stop sessions-postgres from compiling sqlite ([#389](https://github.com/SMK1085/paigasus-helikon/pull/389))

## [0.3.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.3.0...paigasus-helikon-sessions-sqlite-v0.3.1) - 2026-11-21

### Other

- updated the following local packages: paigasus-helikon-core

## [0.3.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.2.0...paigasus-helikon-sessions-sqlite-v0.3.0) - 2026-11-21

### Added

- *(core)* [**breaking**] SMA-830 let a tool declare a provider tool type ([#374](https://github.com/SMK1085/paigasus-helikon/pull/374))

### Upgrade notes

- Requires `paigasus-helikon-core` 0.8 (`ToolDef` is now `#[non_exhaustive]`; see the core CHANGELOG).

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.30...paigasus-helikon-sessions-sqlite-v0.2.0) - 2026-11-21

### Breaking changes

- This version replaces `0.1.30`. Version `0.1.30` requires `paigasus-helikon-core` 0.7, but it was released as a patch, so `cargo update` gave it to users who stay on core 0.6. Version `0.1.30` is yanked. The content of this version is the same as `0.1.30`.
- This version requires `paigasus-helikon-core` 0.7. If you stay on core 0.6, use `0.1.29`, the last version for core 0.6. A requirement cannot select a version below its minimum, so if your requirement is higher than `0.1.29` (for example a requirement that `cargo add` wrote for a yanked version), change it to `"0.1.29"` first, then run `cargo update`.
- If you already use core 0.7, change your requirement on `paigasus-helikon-sessions-sqlite` to `"0.2"`. If you do not, and your requirement allows the core 0.6 versions (for example `"0.1"`), `cargo update` moves you back to the core 0.6 build after the yank, and your build has two copies of core.

## [0.1.30](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.29...paigasus-helikon-sessions-sqlite-v0.1.30) - 2026-11-20

### Other

- update Cargo.toml dependencies

## [0.1.29](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.28...paigasus-helikon-sessions-sqlite-v0.1.29) - 2026-11-20

### Security

- Require `sqlx` 0.9.1 or later ([#351](https://github.com/SMK1085/paigasus-helikon/pull/351)). This addresses RUSTSEC-2026-0459 (CVE-2026-119375): `libsqlite3-sys` before 0.39 bundles an affected SQLite. `sqlx` 0.9.0 caps `libsqlite3-sys` below 0.38, so it cannot resolve a fixed version.
- The new floor does not update an existing lockfile. Run `cargo update -p libsqlite3-sys`, then check that `cargo tree -i libsqlite3-sys` shows 0.39 or later. If another dependency caps it lower, update that dependency and run the update again. Builds with `bundled-sqlcipher` must also check the bundled SQLCipher version.

### Other

- update Cargo.toml dependencies

## [0.1.28](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.27...paigasus-helikon-sessions-sqlite-v0.1.28) - 2026-11-20

### Other

- update Cargo.toml dependencies

## [0.1.27](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.26...paigasus-helikon-sessions-sqlite-v0.1.27) - 2026-11-12

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.26](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.25...paigasus-helikon-sessions-sqlite-v0.1.26) - 2026-09-06

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.25](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.24...paigasus-helikon-sessions-sqlite-v0.1.25) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.24](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.23...paigasus-helikon-sessions-sqlite-v0.1.24) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.23](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.22...paigasus-helikon-sessions-sqlite-v0.1.23) - 2026-08-16

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.22](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.21...paigasus-helikon-sessions-sqlite-v0.1.22) - 2026-08-14

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.21](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.20...paigasus-helikon-sessions-sqlite-v0.1.21) - 2026-07-18

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.20](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.19...paigasus-helikon-sessions-sqlite-v0.1.20) - 2026-07-07

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.19](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.18...paigasus-helikon-sessions-sqlite-v0.1.19) - 2026-07-05

### Fixed

- *(sessions-sqlite)* SMA-431 harden concurrent-writer tests against slow runners and document contention behavior ([#132](https://github.com/SMK1085/paigasus-helikon/pull/132))

## [0.1.18](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.17...paigasus-helikon-sessions-sqlite-v0.1.18) - 2026-06-29

### Added

- *(core)* SMA-330 add CompactingSession, token counting, and shared session conformance suite ([#124](https://github.com/SMK1085/paigasus-helikon/pull/124))

## [0.1.17](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.16...paigasus-helikon-sessions-sqlite-v0.1.17) - 2026-06-21

### Other

- update Cargo.toml dependencies

## [0.1.16](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.15...paigasus-helikon-sessions-sqlite-v0.1.16) - 2026-06-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.15](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.14...paigasus-helikon-sessions-sqlite-v0.1.15) - 2026-06-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.14](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.13...paigasus-helikon-sessions-sqlite-v0.1.14) - 2026-06-18

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.13](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.12...paigasus-helikon-sessions-sqlite-v0.1.13) - 2026-06-17

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.12](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.11...paigasus-helikon-sessions-sqlite-v0.1.12) - 2026-06-16

### Other

- *(repo)* SMA-424 refresh crate READMEs to match the shipped surface ([#93](https://github.com/SMK1085/paigasus-helikon/pull/93))

## [0.1.11](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.10...paigasus-helikon-sessions-sqlite-v0.1.11) - 2026-06-16

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.10](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.9...paigasus-helikon-sessions-sqlite-v0.1.10) - 2026-06-15

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.9](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.8...paigasus-helikon-sessions-sqlite-v0.1.9) - 2026-06-14

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.8](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.7...paigasus-helikon-sessions-sqlite-v0.1.8) - 2026-06-08

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.7](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.6...paigasus-helikon-sessions-sqlite-v0.1.7) - 2026-06-04

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.5...paigasus-helikon-sessions-sqlite-v0.1.6) - 2026-06-04

### Other

- update Cargo.toml dependencies

## [0.1.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.4...paigasus-helikon-sessions-sqlite-v0.1.5) - 2026-06-04

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.3...paigasus-helikon-sessions-sqlite-v0.1.4) - 2026-06-01

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.2...paigasus-helikon-sessions-sqlite-v0.1.3) - 2026-05-31

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.1...paigasus-helikon-sessions-sqlite-v0.1.2) - 2026-05-29

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-sqlite-v0.1.0...paigasus-helikon-sessions-sqlite-v0.1.1) - 2026-05-28

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-sessions-sqlite-v0.1.0) - 2026-05-27

### Added

- *(core)* SMA-318 add MemorySession and SqliteSession backends ([#35](https://github.com/SMK1085/paigasus-helikon/pull/35))

### Other

- *(release)* SMA-382 escape release-plz 0.0.0 trap for sessions-sqlite ([#37](https://github.com/SMK1085/paigasus-helikon/pull/37))
- release ([#36](https://github.com/SMK1085/paigasus-helikon/pull/36))

## [0.0.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-sessions-sqlite-v0.0.0) - 2026-05-27

### Added

- *(core)* SMA-318 add MemorySession and SqliteSession backends ([#35](https://github.com/SMK1085/paigasus-helikon/pull/35))
