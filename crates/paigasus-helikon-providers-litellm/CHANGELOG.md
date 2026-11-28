# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Upgrade notes

- `guardrail_intervened` now maps to `FinishReason::ContentFilter` (was `Other`). A turn with tool calls and this stop reason now fails with `AgentError::ToolCallsNotRun`.

## [0.3.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.3.3...paigasus-helikon-providers-litellm-v0.3.4) - 2026-11-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.3.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.3.2...paigasus-helikon-providers-litellm-v0.3.3) - 2026-11-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.3.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.3.1...paigasus-helikon-providers-litellm-v0.3.2) - 2026-11-22

### Other

- *(providers)* SMA-845 state the provenance of wire-format fixtures ([#384](https://github.com/SMK1085/paigasus-helikon/pull/384))

## [0.3.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.3.0...paigasus-helikon-providers-litellm-v0.3.1) - 2026-11-21

### Fixed

- *(core)* SMA-621 refuse a turn with parallel blank call_id tool calls ([#378](https://github.com/SMK1085/paigasus-helikon/pull/378))

## [0.3.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.2.0...paigasus-helikon-providers-litellm-v0.3.0) - 2026-11-21

### Added

- *(core)* [**breaking**] SMA-830 let a tool declare a provider tool type ([#374](https://github.com/SMK1085/paigasus-helikon/pull/374))

### Fixed

- *(providers)* SMA-827 round trip gemini thought signatures through a provider data part ([#373](https://github.com/SMK1085/paigasus-helikon/pull/373))

### Upgrade notes

- Requires `paigasus-helikon-core` 0.8 (`ToolDef` is now `#[non_exhaustive]`; see the core CHANGELOG).

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.1.11...paigasus-helikon-providers-litellm-v0.2.0) - 2026-11-21

### Breaking changes

- This version replaces `0.1.11`. Version `0.1.11` requires `paigasus-helikon-core` 0.7, but it was released as a patch, so `cargo update` gave it to users who stay on core 0.6. Version `0.1.11` is yanked. The content of this version is the same as `0.1.11`.
- This version requires `paigasus-helikon-core` 0.7. If you stay on core 0.6, use `0.1.10`, the last version for core 0.6. A requirement cannot select a version below its minimum, so if your requirement is higher than `0.1.10` (for example a requirement that `cargo add` wrote for a yanked version), change it to `"0.1.10"` first, then run `cargo update`.
- If you already use core 0.7, change your requirement on `paigasus-helikon-providers-litellm` to `"0.2"`. If you do not, and your requirement allows the core 0.6 versions (for example `"0.1"`), `cargo update` moves you back to the core 0.6 build after the yank, and your build has two copies of core.

## [0.1.11](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.1.10...paigasus-helikon-providers-litellm-v0.1.11) - 2026-11-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.10](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.1.9...paigasus-helikon-providers-litellm-v0.1.10) - 2026-11-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.9](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.1.8...paigasus-helikon-providers-litellm-v0.1.9) - 2026-11-12

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.8](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.1.7...paigasus-helikon-providers-litellm-v0.1.8) - 2026-09-07

### Fixed

- *(providers)* SMA-619 stop litellm splitting a call across two call_ids ([#249](https://github.com/SMK1085/paigasus-helikon/pull/249))

## [0.1.7](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.1.6...paigasus-helikon-providers-litellm-v0.1.7) - 2026-09-06

### Fixed

- *(providers)* SMA-616 stop litellm dropping a blank-id call's name ([#243](https://github.com/SMK1085/paigasus-helikon/pull/243))

## [0.1.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.1.5...paigasus-helikon-providers-litellm-v0.1.6) - 2026-09-06

### Fixed

- *(providers-openai)* SMA-566 alias tool-call indexes onto the owning call_id ([#240](https://github.com/SMK1085/paigasus-helikon/pull/240))

## [0.1.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.1.4...paigasus-helikon-providers-litellm-v0.1.5) - 2026-08-23

### Other

- *(workflows)* SMA-581 gate the published markdown surfaces with markdownlint-cli2 ([#220](https://github.com/SMK1085/paigasus-helikon/pull/220))

## [0.1.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.1.3...paigasus-helikon-providers-litellm-v0.1.4) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.1.2...paigasus-helikon-providers-litellm-v0.1.3) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.1.1...paigasus-helikon-providers-litellm-v0.1.2) - 2026-08-20

### Fixed

- *(providers)* SMA-543 route chat-translator warnings to their declared tracing target ([#209](https://github.com/SMK1085/paigasus-helikon/pull/209))
- *(providers)* SMA-550 canonicalize litellm tool-call correlation on the resolved call_id ([#208](https://github.com/SMK1085/paigasus-helikon/pull/208))

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-litellm-v0.1.0...paigasus-helikon-providers-litellm-v0.1.1) - 2026-08-18

### Fixed

- *(providers)* SMA-547 defer streaming tool-call names until they are complete ([#204](https://github.com/SMK1085/paigasus-helikon/pull/204))
