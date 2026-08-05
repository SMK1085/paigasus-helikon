# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
