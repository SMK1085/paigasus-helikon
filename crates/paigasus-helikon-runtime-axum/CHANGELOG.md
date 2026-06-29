# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-runtime-axum-v0.1.0) - 2026-06-30

### Added

- *(runtime-axum)* SMA-331 initial real implementation: HTTP/SSE/WebSocket AgentServer runtime ([#128](https://github.com/SMK1085/paigasus-helikon/pull/128))
  - One-shot POST `/agents/{id}/run`, SSE streaming `/agents/{id}/run/stream`, and async `/agents/{id}/run/async` endpoints
  - WebSocket replay via `/agents/{id}/runs/{run_id}/ws`
  - `GET /agents` registry listing
  - OpenAPI schema generation behind the `openapi` feature (backed by utoipa)
  - `SessionProvider`, `RunContextProvider`, and `AuthProvider` traits for pluggable backends
  - Replayable runs with configurable TTL and retention count

## [0.0.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-runtime-axum-v0.0.0) - 2026-05-17

### Added

- *(runtime)* SMA-304 add tokio, axum, temporal, agentcore runtime stubs

### Other

- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))
