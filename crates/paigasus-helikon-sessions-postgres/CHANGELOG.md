# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.10](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-postgres-v0.1.9...paigasus-helikon-sessions-postgres-v0.1.10) - 2026-11-20

### Security

- Require `sqlx` 0.9.1 or later ([#351](https://github.com/SMK1085/paigasus-helikon/pull/351)). This addresses RUSTSEC-2026-0459 (CVE-2026-119375): `libsqlite3-sys` before 0.39 bundles an affected SQLite. `sqlx` 0.9.0 caps `libsqlite3-sys` below 0.38, so it cannot resolve a fixed version.
- The new floor does not update an existing lockfile. Run `cargo update -p libsqlite3-sys`, then check that `cargo tree -i libsqlite3-sys` shows 0.39 or later. If another dependency caps it lower, update that dependency and run the update again. Builds with `bundled-sqlcipher` must also check the bundled SQLCipher version.

### Other

- update Cargo.toml dependencies

## [0.1.9](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-postgres-v0.1.8...paigasus-helikon-sessions-postgres-v0.1.9) - 2026-11-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.8](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-postgres-v0.1.7...paigasus-helikon-sessions-postgres-v0.1.8) - 2026-11-12

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.7](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-postgres-v0.1.6...paigasus-helikon-sessions-postgres-v0.1.7) - 2026-09-06

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-postgres-v0.1.5...paigasus-helikon-sessions-postgres-v0.1.6) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-postgres-v0.1.4...paigasus-helikon-sessions-postgres-v0.1.5) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-postgres-v0.1.3...paigasus-helikon-sessions-postgres-v0.1.4) - 2026-08-16

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-postgres-v0.1.2...paigasus-helikon-sessions-postgres-v0.1.3) - 2026-08-14

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-postgres-v0.1.1...paigasus-helikon-sessions-postgres-v0.1.2) - 2026-07-18

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-postgres-v0.1.0...paigasus-helikon-sessions-postgres-v0.1.1) - 2026-07-07

### Other

- updated the following local packages: paigasus-helikon-core
