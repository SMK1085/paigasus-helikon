# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.4.3...paigasus-helikon-runtime-agentcore-v0.4.4) - 2026-11-22

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio, paigasus-helikon-mcp, paigasus-helikon-providers-anthropic, paigasus-helikon-runtime-axum

## [0.4.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.4.2...paigasus-helikon-runtime-agentcore-v0.4.3) - 2026-11-22

### Other

- updated the following local packages: paigasus-helikon-providers-anthropic

## [0.4.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.4.1...paigasus-helikon-runtime-agentcore-v0.4.2) - 2026-11-22

### Other

- updated the following local packages: paigasus-helikon-providers-anthropic

## [0.4.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.4.0...paigasus-helikon-runtime-agentcore-v0.4.1) - 2026-11-21

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio, paigasus-helikon-mcp, paigasus-helikon-providers-anthropic, paigasus-helikon-runtime-axum

## [0.4.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.3.0...paigasus-helikon-runtime-agentcore-v0.4.0) - 2026-11-21

### Added

- *(core)* [**breaking**] SMA-830 let a tool declare a provider tool type ([#374](https://github.com/SMK1085/paigasus-helikon/pull/374))

### Fixed

- *(providers)* SMA-827 round trip gemini thought signatures through a provider data part ([#373](https://github.com/SMK1085/paigasus-helikon/pull/373))

### Upgrade notes

- Requires `paigasus-helikon-core` 0.8 (`ToolDef` is now `#[non_exhaustive]`; see the core CHANGELOG).

## [0.3.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.17...paigasus-helikon-runtime-agentcore-v0.3.0) - 2026-11-21

### Breaking changes

- This version replaces `0.2.16` and `0.2.17`. These versions require `paigasus-helikon-core` 0.7, but they were released as patches, so `cargo update` gave them to users who stay on core 0.6. Both versions are yanked. The content of this version is the same as `0.2.17`.
- This version requires `paigasus-helikon-core` 0.7. If you stay on core 0.6, use `0.2.15`, the last version for core 0.6. A requirement cannot select a version below its minimum, so if your requirement is higher than `0.2.15` (for example a requirement that `cargo add` wrote for a yanked version), change it to `"0.2.15"` first, then run `cargo update`.
- If you already use core 0.7, change your requirement on `paigasus-helikon-runtime-agentcore` to `"0.3"`. If you do not, and your requirement allows the core 0.6 versions (for example `"0.2"`), `cargo update` moves you back to the core 0.6 build after the yank, and your build has two copies of core.

## [0.2.17](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.16...paigasus-helikon-runtime-agentcore-v0.2.17) - 2026-11-20

### Other

- updated the following local packages: paigasus-helikon-providers-anthropic

## [0.2.16](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.15...paigasus-helikon-runtime-agentcore-v0.2.16) - 2026-11-20

### Fixed

- *(core)* SMA-810 give each handoff transfer call a tool result ([#358](https://github.com/SMK1085/paigasus-helikon/pull/358))

## [0.2.15](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.14...paigasus-helikon-runtime-agentcore-v0.2.15) - 2026-11-20

### Other

- updated the following local packages: paigasus-helikon-providers-anthropic

## [0.2.14](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.13...paigasus-helikon-runtime-agentcore-v0.2.14) - 2026-11-20

### Added

- *(core)* SMA-804 add a turn-boundary checkpoint to runcontext ([#346](https://github.com/SMK1085/paigasus-helikon/pull/346))
- *(core)* SMA-803 add deferred tool calls ([#344](https://github.com/SMK1085/paigasus-helikon/pull/344))

## [0.2.13](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.12...paigasus-helikon-runtime-agentcore-v0.2.13) - 2026-11-12

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio, paigasus-helikon-mcp, paigasus-helikon-providers-anthropic, paigasus-helikon-runtime-axum

## [0.2.12](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.11...paigasus-helikon-runtime-agentcore-v0.2.12) - 2026-10-01

### Other

- *(deps)* replace yanked yoke-derive, fix a rust 1.99 rustdoc lint ([#284](https://github.com/SMK1085/paigasus-helikon/pull/284))

## [0.2.11](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.10...paigasus-helikon-runtime-agentcore-v0.2.11) - 2026-09-17

### Other

- updated the following local packages: paigasus-helikon-mcp

## [0.2.10](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.9...paigasus-helikon-runtime-agentcore-v0.2.10) - 2026-09-06

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio, paigasus-helikon-mcp, paigasus-helikon-providers-anthropic, paigasus-helikon-runtime-axum

## [0.2.9](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.8...paigasus-helikon-runtime-agentcore-v0.2.9) - 2026-08-23

### Other

- updated the following local packages: paigasus-helikon-runtime-axum

## [0.2.8](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.7...paigasus-helikon-runtime-agentcore-v0.2.8) - 2026-08-22

### Added

- *(core)* SMA-568 adopt paigasus:: tracing targets in core and the runtimes ([#216](https://github.com/SMK1085/paigasus-helikon/pull/216))

## [0.2.7](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.6...paigasus-helikon-runtime-agentcore-v0.2.7) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-runtime-tokio, paigasus-helikon-runtime-axum

## [0.2.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.5...paigasus-helikon-runtime-agentcore-v0.2.6) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio, paigasus-helikon-mcp, paigasus-helikon-providers-anthropic, paigasus-helikon-runtime-axum

## [0.2.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.4...paigasus-helikon-runtime-agentcore-v0.2.5) - 2026-08-17

### Other

- updated the following local packages: paigasus-helikon-providers-anthropic

## [0.2.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.3...paigasus-helikon-runtime-agentcore-v0.2.4) - 2026-08-16

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio, paigasus-helikon-mcp, paigasus-helikon-providers-anthropic, paigasus-helikon-runtime-axum

## [0.2.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.2...paigasus-helikon-runtime-agentcore-v0.2.3) - 2026-08-14

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio, paigasus-helikon-runtime-axum, paigasus-helikon-mcp, paigasus-helikon-providers-anthropic

## [0.2.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.1...paigasus-helikon-runtime-agentcore-v0.2.2) - 2026-08-13

### Added

- *(runtime-agentcore)* SMA-461 add a2a and ag-ui protocol modes ([#186](https://github.com/SMK1085/paigasus-helikon/pull/186))

## [0.2.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.2.0...paigasus-helikon-runtime-agentcore-v0.2.1) - 2026-08-07

### Other

- *(workflows)* SMA-457 add temporal-it and agentcore-image integration jobs ([#181](https://github.com/SMK1085/paigasus-helikon/pull/181))

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.1.6...paigasus-helikon-runtime-agentcore-v0.2.0) - 2026-08-07

### Added

- *(runtime)* [**breaking**] SMA-482 harden 5xx redaction, session-principal binding, and run caps ([#177](https://github.com/SMK1085/paigasus-helikon/pull/177))

## [0.1.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.1.5...paigasus-helikon-runtime-agentcore-v0.1.6) - 2026-08-04

### Other

- updated the following local packages: paigasus-helikon-runtime-axum

## [0.1.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.1.4...paigasus-helikon-runtime-agentcore-v0.1.5) - 2026-08-04

### Other

- *(deps)* bump rmcp to 3.1 ([#170](https://github.com/SMK1085/paigasus-helikon/pull/170))

## [0.1.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.1.3...paigasus-helikon-runtime-agentcore-v0.1.4) - 2026-07-18

### Fixed

- *(runtime-agentcore)* SMA-456 finalize one-shot json runs on client disconnect ([#150](https://github.com/SMK1085/paigasus-helikon/pull/150))

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.1.2...paigasus-helikon-runtime-agentcore-v0.1.3) - 2026-07-13

### Other

- updated the following local packages: paigasus-helikon-runtime-axum

## [0.1.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.1.1...paigasus-helikon-runtime-agentcore-v0.1.2) - 2026-07-12

### Other

- update Cargo.toml dependencies

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-agentcore-v0.1.0...paigasus-helikon-runtime-agentcore-v0.1.1) - 2026-07-07

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio, paigasus-helikon-mcp, paigasus-helikon-providers-anthropic, paigasus-helikon-runtime-axum

## [0.1.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-runtime-agentcore-v0.1.0) - 2026-07-06

### Added

- *(runtime-agentcore)* SMA-332 initial real implementation: AWS Bedrock AgentCore HTTP runtime
  - `AgentCoreServer<Ctx>` — an axum app implementing the AgentCore contract: `GET /ping` health check, `POST /invocations` for agent runs
  - `/invocations` supports both a buffered JSON response and an SSE streaming mode, selected per-request
  - Session handling matched to AgentCore's platform-injected session id/header conventions
  - Runs finalize and cancel cleanly on client disconnect for SSE mode
  - Optional `mcp` feature (on by default): exposes the configured agent as an MCP server over rmcp's stateless streamable-HTTP transport (`paigasus-helikon-mcp`'s `streamable_http_service_with`), for AgentCore's MCP-protocol mode
  - `examples/agent_http.rs` (behind `example-anthropic`) and `examples/mcp_server.rs` (behind `mcp`)
  - `docker/` — an arm64 Dockerfile building a deployable AgentCore container image, plus `scripts/agentcore-image-check.sh` gating image size and cold-start latency for both the plain-HTTP and MCP images

## [0.0.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-runtime-agentcore-v0.0.0) - 2026-05-17

### Added

- *(runtime)* SMA-304 add tokio, axum, temporal, agentcore runtime stubs

### Other

- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))
