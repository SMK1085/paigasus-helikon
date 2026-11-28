# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.4.0...paigasus-helikon-providers-anthropic-v0.4.1) - 2026-11-28

### Fixed

- *(core)* SMA-864 do not run tool calls from a cut or filtered model turn ([#406](https://github.com/SMK1085/paigasus-helikon/pull/406))

### Upgrade notes

- `model_context_window_exceeded` now maps to `FinishReason::Length` (was `Other`). A turn with tool calls and this stop reason now fails with `AgentError::ToolCallsNotRun`.

## [0.4.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.3.6...paigasus-helikon-providers-anthropic-v0.4.0) - 2026-11-27

### Added

- *(providers-anthropic)* [**breaking**] SMA-868 add a `rustls-no-provider` feature and map http 4xx to `Rejected`
- The features `rustls-aws-lc` and `rustls-no-provider`. `rustls-aws-lc` is the default feature and uses the aws-lc rustls provider, as before. With `rustls-no-provider`, the crate has no rustls provider. The host must install a rustls `CryptoProvider` as the process default before `build()`, or pass its own client with `http_client(...)`.
- `BuildError::NoCryptoProvider`, `BuildError::HttpClient` and `BuildError::InvalidAuthCredential`.
- The client that `build()` makes always uses the rustls backend.

### Fixed

- *(providers)* add rows for the current claude 5 models and fix the haiku 4.5 facts ([#403](https://github.com/SMK1085/paigasus-helikon/pull/403))
- The API key is redacted in the `Debug` text, and the client sends it as a sensitive header.
- A bad API key fails at `build()` with `BuildError::InvalidAuthCredential`. Before, it caused a panic in `invoke()`.

### Upgrade notes

- **Breaking:** an HTTP 401 or 403 now gives `ModelError::Rejected`, not `Refused`. Code that matches `Refused` for an auth failure must match `Rejected { status: 401 | 403, .. }`.
- **Breaking:** an HTTP 400, 404 or 413 now gives `Rejected`, not `Other`. An HTTP 429 with `billing_error` now gives `RateLimited` with the `retry-after` value, not `Other`.
- An HTTP 402, 409 or 422 with an auth error type (`authentication_error` or `permission_error`) now gives `ModelError::Other` (before: `Refused`).
- `BuildError` has three new variants. It is `#[non_exhaustive]`, so a `match` with a wildcard arm still compiles.
- **Breaking:** the new default feature `rustls-aws-lc` selects the TLS backend. With `default-features = false`, the crate selects no TLS backend. Enable `rustls-aws-lc` or `rustls-no-provider`.
- `claude-fable-5-2`, `claude-fable-5-1`, `claude-fable-5`, `claude-opus-5-5`, `claude-opus-5`, `claude-opus-4-8`, `claude-sonnet-5-5` and `claude-sonnet-5` now report streaming, tools, parallel tool calls, structured output, vision, reasoning and prompt caching. Their default `max_tokens` goes from 4096 to 32768. To keep a lower limit, set `max_output_tokens` per request.
- `claude-haiku-4-5` now reports reasoning.
- Claude Opus 4.1 is retired. A `claude-opus-4-1` id now reports the conservative capability set, with a default `max_tokens` of 4096.
- Requires `paigasus-helikon-core` 0.8.4.

## [0.3.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.3.5...paigasus-helikon-providers-anthropic-v0.3.6) - 2026-11-26

### Fixed

- *(providers)* SMA-863 emit a tool call delta for a tool call with no input delta ([#402](https://github.com/SMK1085/paigasus-helikon/pull/402))
- *(providers)* SMA-850 replace the retired claude 3.x model ids ([#399](https://github.com/SMK1085/paigasus-helikon/pull/399))

### Upgrade notes

- Claude 3.x models are not supported. A `claude-3-*` id now reports the conservative capability set (streaming and tools). For a `claude-3-5-*` id, the default `max_tokens` drops from 8192 to 4096. Claude 3 ids already had 4096. If you still call such a model, set `max_output_tokens` per request and `.with_capabilities()` on the builder, or stay on 0.3.5.
- `claude-haiku-5-5` now reports streaming, tools, parallel tool calls, structured output, vision, reasoning and prompt caching, with a default `max_tokens` of 32768.

## [0.3.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.3.4...paigasus-helikon-providers-anthropic-v0.3.5) - 2026-11-22

### Fixed

- *(providers)* SMA-854 send each anthropic text block at its own position ([#393](https://github.com/SMK1085/paigasus-helikon/pull/393))

## [0.3.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.3.3...paigasus-helikon-providers-anthropic-v0.3.4) - 2026-11-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.3.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.3.2...paigasus-helikon-providers-anthropic-v0.3.3) - 2026-11-22

### Other

- *(providers)* SMA-845 state the provenance of wire-format fixtures ([#384](https://github.com/SMK1085/paigasus-helikon/pull/384))

## [0.3.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.3.1...paigasus-helikon-providers-anthropic-v0.3.2) - 2026-11-22

### Added

- *(providers)* SMA-840 round trip anthropic thinking signatures and add effort ([#387](https://github.com/SMK1085/paigasus-helikon/pull/387))

## [0.3.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.3.0...paigasus-helikon-providers-anthropic-v0.3.1) - 2026-11-21

### Other

- updated the following local packages: paigasus-helikon-core

## [0.3.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.2.0...paigasus-helikon-providers-anthropic-v0.3.0) - 2026-11-21

### Added

- *(core)* [**breaking**] SMA-830 let a tool declare a provider tool type ([#374](https://github.com/SMK1085/paigasus-helikon/pull/374))

### Fixed

- *(providers)* SMA-827 round trip gemini thought signatures through a provider data part ([#373](https://github.com/SMK1085/paigasus-helikon/pull/373))

### Upgrade notes

- Requires `paigasus-helikon-core` 0.8 (`ToolDef` is now `#[non_exhaustive]`; see the core CHANGELOG).
- If a tool declares an `anthropic` provider tool type, the adapter now sends the typed entry, for example `{"type": "memory_20250818", "name": "memory"}`, with no description and no input schema. To send the generic schema instead, call `native_tool_types(false)` on the builder.

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.30...paigasus-helikon-providers-anthropic-v0.2.0) - 2026-11-21

### Breaking changes

- This version replaces `0.1.29` and `0.1.30`. These versions require `paigasus-helikon-core` 0.7, but they were released as patches, so `cargo update` gave them to users who stay on core 0.6. Both versions are yanked. The content of this version is the same as `0.1.30`.
- This version requires `paigasus-helikon-core` 0.7. If you stay on core 0.6, use `0.1.28`, the last version for core 0.6. A requirement cannot select a version below its minimum, so if your requirement is higher than `0.1.28` (for example a requirement that `cargo add` wrote for a yanked version), change it to `"0.1.28"` first, then run `cargo update`.
- If you already use core 0.7, change your requirement on `paigasus-helikon-providers-anthropic` to `"0.2"`. If you do not, and your requirement allows the core 0.6 versions (for example `"0.1"`), `cargo update` moves you back to the core 0.6 build after the yank, and your build has two copies of core.

## [0.1.30](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.29...paigasus-helikon-providers-anthropic-v0.1.30) - 2026-11-20

### Fixed

- *(providers)* SMA-834 let tool-use-only assistant messages join an open anthropic round ([#368](https://github.com/SMK1085/paigasus-helikon/pull/368))

## [0.1.29](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.28...paigasus-helikon-providers-anthropic-v0.1.29) - 2026-11-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.28](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.27...paigasus-helikon-providers-anthropic-v0.1.28) - 2026-11-20

### Fixed

- *(providers-anthropic)* SMA-819 drop empty text blocks and keep tool rounds in order ([#353](https://github.com/SMK1085/paigasus-helikon/pull/353))

## [0.1.27](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.26...paigasus-helikon-providers-anthropic-v0.1.27) - 2026-11-20

### Added

- *(core)* SMA-804 add a turn-boundary checkpoint to runcontext ([#346](https://github.com/SMK1085/paigasus-helikon/pull/346))

## [0.1.26](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.25...paigasus-helikon-providers-anthropic-v0.1.26) - 2026-11-12

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.25](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.24...paigasus-helikon-providers-anthropic-v0.1.25) - 2026-09-06

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.24](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.23...paigasus-helikon-providers-anthropic-v0.1.24) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.23](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.22...paigasus-helikon-providers-anthropic-v0.1.23) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.22](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.21...paigasus-helikon-providers-anthropic-v0.1.22) - 2026-08-17

### Fixed

- *(providers-anthropic)* SMA-531 emit finish when a stream body ends before message_stop ([#200](https://github.com/SMK1085/paigasus-helikon/pull/200))

## [0.1.21](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.20...paigasus-helikon-providers-anthropic-v0.1.21) - 2026-08-16

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.20](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.19...paigasus-helikon-providers-anthropic-v0.1.20) - 2026-08-14

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.19](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.18...paigasus-helikon-providers-anthropic-v0.1.19) - 2026-07-18

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.18](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.17...paigasus-helikon-providers-anthropic-v0.1.18) - 2026-07-07

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.17](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.16...paigasus-helikon-providers-anthropic-v0.1.17) - 2026-06-29

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.16](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.15...paigasus-helikon-providers-anthropic-v0.1.16) - 2026-06-21

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.15](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.14...paigasus-helikon-providers-anthropic-v0.1.15) - 2026-06-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.14](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.13...paigasus-helikon-providers-anthropic-v0.1.14) - 2026-06-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.13](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.12...paigasus-helikon-providers-anthropic-v0.1.13) - 2026-06-18

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.12](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.11...paigasus-helikon-providers-anthropic-v0.1.12) - 2026-06-17

### Added

- *(runtime-tokio)* SMA-393 add RetryingModel retry decorator for transient errors ([#97](https://github.com/SMK1085/paigasus-helikon/pull/97))

## [0.1.11](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.10...paigasus-helikon-providers-anthropic-v0.1.11) - 2026-06-16

### Other

- *(repo)* SMA-424 refresh crate READMEs to match the shipped surface ([#93](https://github.com/SMK1085/paigasus-helikon/pull/93))

## [0.1.10](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.9...paigasus-helikon-providers-anthropic-v0.1.10) - 2026-06-16

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.9](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.8...paigasus-helikon-providers-anthropic-v0.1.9) - 2026-06-15

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.8](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.7...paigasus-helikon-providers-anthropic-v0.1.8) - 2026-06-14

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.7](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.6...paigasus-helikon-providers-anthropic-v0.1.7) - 2026-06-08

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.5...paigasus-helikon-providers-anthropic-v0.1.6) - 2026-06-04

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.4...paigasus-helikon-providers-anthropic-v0.1.5) - 2026-06-04

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.3...paigasus-helikon-providers-anthropic-v0.1.4) - 2026-06-01

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.2...paigasus-helikon-providers-anthropic-v0.1.3) - 2026-05-31

### Added

- *(core)* SMA-322 emit opentelemetry genai-semconv spans ([#51](https://github.com/SMK1085/paigasus-helikon/pull/51))

## [0.1.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.1...paigasus-helikon-providers-anthropic-v0.1.2) - 2026-05-29

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-providers-anthropic-v0.1.0...paigasus-helikon-providers-anthropic-v0.1.1) - 2026-05-28

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-providers-anthropic-v0.1.0) - 2026-05-26

### Added

- *(providers-anthropic)* SMA-317 add Anthropic provider with streaming, tools, caching ([#33](https://github.com/SMK1085/paigasus-helikon/pull/33))
- *(providers)* SMA-304 add openai and anthropic provider stubs

### Other

- release v0.0.0 ([#6](https://github.com/SMK1085/paigasus-helikon/pull/6))
- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))

## [0.0.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-providers-anthropic-v0.0.0) - 2026-05-17

### Added

- *(providers)* SMA-304 add openai and anthropic provider stubs

### Other

- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))
