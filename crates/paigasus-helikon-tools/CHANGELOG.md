# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.9](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.2.8...paigasus-helikon-tools-v0.2.9) - 2026-06-29

### Other

- updated the following local packages: paigasus-helikon-core

## [0.2.8](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.2.7...paigasus-helikon-tools-v0.2.8) - 2026-06-24

### Added

- *(tools)* SMA-447 reap orphaned forkd microVMs via reconcile() ([#118](https://github.com/SMK1085/paigasus-helikon/pull/118))

## [0.2.7](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.2.6...paigasus-helikon-tools-v0.2.7) - 2026-06-23

### Added

- *(tools)* SMA-437 enforce forkd microVM egress + add live-KVM validation harness ([#114](https://github.com/SMK1085/paigasus-helikon/pull/114))

## [0.2.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.2.5...paigasus-helikon-tools-v0.2.6) - 2026-06-21

### Added

- *(tools)* SMA-416 add forkd microVM ExecutionBackend skeleton + spike note ([#107](https://github.com/SMK1085/paigasus-helikon/pull/107))

## [0.2.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.2.4...paigasus-helikon-tools-v0.2.5) - 2026-06-20

### Added

- *(core)* SMA-403 add runcontext ephemeral constructor and dependency setters ([#105](https://github.com/SMK1085/paigasus-helikon/pull/105))

## [0.2.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.2.3...paigasus-helikon-tools-v0.2.4) - 2026-06-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.2.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.2.2...paigasus-helikon-tools-v0.2.3) - 2026-06-18

### Added

- *(core)* SMA-414 add operator-aware deny matching, destructive breaker, and output redaction ([#101](https://github.com/SMK1085/paigasus-helikon/pull/101))

## [0.2.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.2.1...paigasus-helikon-tools-v0.2.2) - 2026-06-17

### Added

- *(tools)* SMA-426 add macOS Seatbelt ExecutionBackend for Bash ([#99](https://github.com/SMK1085/paigasus-helikon/pull/99))

### Added

- *(tools)* SMA-426 add a macOS Seatbelt `OsSandboxBackend` (via `sandbox-exec`) behind the existing `os-sandbox` feature — write-focused OS containment (deny-by-default; writes outside the sandbox root denied at the OS layer; reads unrestricted; all-or-nothing network), same API as the Linux backend.

## [0.2.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.2.0...paigasus-helikon-tools-v0.2.1) - 2026-06-17

### Other

- updated the following local packages: paigasus-helikon-core

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.1.6...paigasus-helikon-tools-v0.2.0) - 2026-06-17

### Added

- *(tools)* [**breaking**] SMA-413 add pluggable ExecutionBackend with Host and OS-sandbox backends ([#95](https://github.com/SMK1085/paigasus-helikon/pull/95))

## [0.1.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.1.5...paigasus-helikon-tools-v0.1.6) - 2026-06-16

### Other

- *(repo)* SMA-424 refresh crate READMEs to match the shipped surface ([#93](https://github.com/SMK1085/paigasus-helikon/pull/93))

## [0.1.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.1.4...paigasus-helikon-tools-v0.1.5) - 2026-06-16

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.1.3...paigasus-helikon-tools-v0.1.4) - 2026-06-15

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.1.2...paigasus-helikon-tools-v0.1.3) - 2026-06-14

### Added

- *(core)* SMA-418 add atomic increment_u64_if_below for exact max_uses cap ([#80](https://github.com/SMK1085/paigasus-helikon/pull/80))

## [0.1.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.1.1...paigasus-helikon-tools-v0.1.2) - 2026-06-14

### Added

- *(tools)* SMA-417 finish WebFetch SSRF hardening + WebSearch domain filter ([#78](https://github.com/SMK1085/paigasus-helikon/pull/78))

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-tools-v0.1.0...paigasus-helikon-tools-v0.1.1) - 2026-06-14

### Added

- *(tools)* SMA-412 add WebFetch + WebSearch network tools ([#76](https://github.com/SMK1085/paigasus-helikon/pull/76))

## [0.1.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-tools-v0.1.0) - 2026-06-13

### Added

- *(tools)* SMA-328 sandboxed filesystem and process tools — `Sandbox` (cap-std), `ReadTool`, `WriteTool`, `EditTool`, `BashTool`; new `ToolError::Denied` boundary-violation error.

## [0.0.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-tools-v0.0.0) - 2026-05-17

### Added

- *(mcp,tools)* SMA-304 add mcp and tools stub crates

### Other

- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))
