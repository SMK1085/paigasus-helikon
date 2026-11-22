# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.3.2...paigasus-helikon-providers-bedrock-v0.3.3) - 2026-11-22

### Added

- *(providers)* SMA-837 add a family override to the bedrock builder ([#383](https://github.com/SMK1085/paigasus-helikon/pull/383))

### Fixed

- *(providers)* SMA-833 send a valid tool history when tool_choice is none ([#386](https://github.com/SMK1085/paigasus-helikon/pull/386))

## [0.3.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.3.1...paigasus-helikon-providers-bedrock-v0.3.2) - 2026-11-21

### Fixed

- *(providers)* SMA-839 send native structured output to bedrock models that reject a forced tool choice ([#379](https://github.com/SMK1085/paigasus-helikon/pull/379))

## [0.3.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.3.0...paigasus-helikon-providers-bedrock-v0.3.1) - 2026-11-21

### Other

- updated the following local packages: paigasus-helikon-core

## [0.3.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.2.0...paigasus-helikon-providers-bedrock-v0.3.0) - 2026-11-21

### Added

- *(core)* [**breaking**] SMA-830 let a tool declare a provider tool type ([#374](https://github.com/SMK1085/paigasus-helikon/pull/374))

### Fixed

- *(providers)* SMA-827 round trip gemini thought signatures through a provider data part ([#373](https://github.com/SMK1085/paigasus-helikon/pull/373))

### Upgrade notes

- Requires `paigasus-helikon-core` 0.8 (`ToolDef` is now `#[non_exhaustive]`; see the core CHANGELOG).

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.1.11...paigasus-helikon-providers-bedrock-v0.2.0) - 2026-11-21

### Breaking changes

- This version replaces `0.1.11`. Version `0.1.11` requires `paigasus-helikon-core` 0.7, but it was released as a patch, so `cargo update` gave it to users who stay on core 0.6. Version `0.1.11` is yanked. This version also contains the fix below, which `0.1.11` does not have.
- This version requires `paigasus-helikon-core` 0.7. If you stay on core 0.6, use `0.1.10`, the last version for core 0.6. A requirement cannot select a version below its minimum, so if your requirement is higher than `0.1.10` (for example a requirement that `cargo add` wrote for a yanked version), change it to `"0.1.10"` first, then run `cargo update`.
- If you already use core 0.7, change your requirement on `paigasus-helikon-providers-bedrock` to `"0.2"`. If you do not, and your requirement allows the core 0.6 versions (for example `"0.1"`), `cargo update` moves you back to the core 0.6 build after the yank, and your build has two copies of core.

### Fixed

- *(providers)* SMA-832 detect the bedrock family behind global, jp, au, ca and in prefixes and in ARNs ([#370](https://github.com/SMK1085/paigasus-helikon/pull/370))

## [0.1.11](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.1.10...paigasus-helikon-providers-bedrock-v0.1.11) - 2026-11-20

### Fixed

- *(providers)* SMA-824 drop blank text blocks and keep tool rounds in order in bedrock ([#365](https://github.com/SMK1085/paigasus-helikon/pull/365))

## [0.1.10](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.1.9...paigasus-helikon-providers-bedrock-v0.1.10) - 2026-11-20

### Added

- *(core)* SMA-804 add a turn-boundary checkpoint to runcontext ([#346](https://github.com/SMK1085/paigasus-helikon/pull/346))

### Fixed

- *(providers)* SMA-817 move bedrock to aws-config behavior version v2026_11_17 ([#347](https://github.com/SMK1085/paigasus-helikon/pull/347))

## [0.1.9](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.1.8...paigasus-helikon-providers-bedrock-v0.1.9) - 2026-11-12

### Fixed

- *(providers)* SMA-766 move bedrock to aws-config behaviorversion v2026_11_10 ([#335](https://github.com/SMK1085/paigasus-helikon/pull/335))

## [0.1.8](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.1.7...paigasus-helikon-providers-bedrock-v0.1.8) - 2026-09-06

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.7](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.1.6...paigasus-helikon-providers-bedrock-v0.1.7) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.1.5...paigasus-helikon-providers-bedrock-v0.1.6) - 2026-08-22

### Fixed

- *(providers)* SMA-533 add a cross-provider stream conformance suite and fix the responses finish reason ([#211](https://github.com/SMK1085/paigasus-helikon/pull/211))

## [0.1.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.1.4...paigasus-helikon-providers-bedrock-v0.1.5) - 2026-08-16

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.1.3...paigasus-helikon-providers-bedrock-v0.1.4) - 2026-08-14

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.1.2...paigasus-helikon-providers-bedrock-v0.1.3) - 2026-07-18

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.1.1...paigasus-helikon-providers-bedrock-v0.1.2) - 2026-07-07

### Added

- *(evals)* SMA-333 add evals crate, cli subcommands, and swarm/graph agents ([#138](https://github.com/SMK1085/paigasus-helikon/pull/138))

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-bedrock-v0.1.0...paigasus-helikon-providers-bedrock-v0.1.1) - 2026-06-29

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-providers-bedrock-v0.1.0) - 2026-06-25

### Added

- *(providers-bedrock)* SMA-329 initial Amazon Bedrock Converse provider — `BedrockModel`, `BedrockModelBuilder`, `BuildError`, `ModelFamily`, family-gated structured output via forced-tool synthesis, `rewrite_tool_schema`/`Ruleset` tool-input schema rewriter, streaming via `ConverseStream`, and `from_env` constructor for the standard AWS credential chain
