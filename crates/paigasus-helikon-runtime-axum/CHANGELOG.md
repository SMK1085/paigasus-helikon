# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.1.0...paigasus-helikon-runtime-axum-v0.1.1) - 2026-07-01

### Added

- *(runtime-axum)* SMA-452 add streaming start-error frame, no-default-features CI gate, and O(n) replay ([#130](https://github.com/SMK1085/paigasus-helikon/pull/130))

## [0.1.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-runtime-axum-v0.1.0) - 2026-06-30

### Added

- *(runtime-axum)* SMA-331 initial real implementation: HTTP/SSE/WebSocket AgentServer runtime ([#129](https://github.com/SMK1085/paigasus-helikon/pull/129))
  - `AgentServer<Ctx>` builder mounting `Arc<dyn Agent<Ctx>>`s; default runner `TokioRunner`
  - `POST /agents/{name}/runs` — one-shot (blocks, returns aggregated `RunResponse` + `X-Run-Id` header)
  - `POST /agents/{name}/runs?stream=sse` — SSE live stream of `AgentEvent` frames
  - `POST /agents/{name}/runs?mode=async` — `202 Accepted` + `run_id`, detached background run
  - `GET /agents/{name}/runs/{id}/events` — WebSocket replay from start then live-tail
  - `GET /agents` — list all mounted agents and their descriptions
  - `GET /openapi.json` — OpenAPI 3.1 schema (behind the default-on `openapi` feature, backed by utoipa)
  - Replayable runs: in-memory run registry with bounded per-run event log, configurable TTL and count-cap retention
  - `X-Session-Id` session affinity with per-session run serialisation; pluggable `SessionProvider` (default bounded in-memory `InMemorySessionProvider` backed by `MemorySession`)
  - Pluggable `ContextProvider` trait (convenience `.with_default_context()` for `Ctx: Default`; the security seam for permission mode and approval handler)
  - Optional `AuthLayer` middleware trait (no built-in implementation; omitting it admits all requests)
  - Cancellation: one-shot and SSE runs cancel on client disconnect; async runs are detached and outlive the connection

## [0.0.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-runtime-axum-v0.0.0) - 2026-05-17

### Added

- *(runtime)* SMA-304 add tokio, axum, temporal, agentcore runtime stubs

### Other

- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))
