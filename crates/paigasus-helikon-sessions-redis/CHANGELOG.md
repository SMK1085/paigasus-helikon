# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-redis-v0.1.10...paigasus-helikon-sessions-redis-v0.2.0) - 2026-11-20

### Breaking changes

- This version replaces `0.1.10`. Version `0.1.10` requires `paigasus-helikon-core` 0.7, but it was released as a patch, so `cargo update` gave it to users who stay on core 0.6. Version `0.1.10` is yanked. The content of this version is the same as `0.1.10`.
- This version requires `paigasus-helikon-core` 0.7. If you stay on core 0.6, keep the previous version. After the yank, `cargo update` selects it.
- If you already use core 0.7, change your requirement on `paigasus-helikon-sessions-redis` to `"0.2"`. If you do not, `cargo update` moves you back to the core 0.6 build after the yank, and your build has two copies of core.

## [0.1.10](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-redis-v0.1.9...paigasus-helikon-sessions-redis-v0.1.10) - 2026-11-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.9](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-redis-v0.1.8...paigasus-helikon-sessions-redis-v0.1.9) - 2026-11-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.8](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-redis-v0.1.7...paigasus-helikon-sessions-redis-v0.1.8) - 2026-11-12

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.7](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-redis-v0.1.6...paigasus-helikon-sessions-redis-v0.1.7) - 2026-09-06

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-redis-v0.1.5...paigasus-helikon-sessions-redis-v0.1.6) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-redis-v0.1.4...paigasus-helikon-sessions-redis-v0.1.5) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-redis-v0.1.3...paigasus-helikon-sessions-redis-v0.1.4) - 2026-08-16

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-redis-v0.1.2...paigasus-helikon-sessions-redis-v0.1.3) - 2026-08-14

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-redis-v0.1.1...paigasus-helikon-sessions-redis-v0.1.2) - 2026-07-18

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-sessions-redis-v0.1.0...paigasus-helikon-sessions-redis-v0.1.1) - 2026-07-07

### Other

- updated the following local packages: paigasus-helikon-core
