# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.4.3...paigasus-helikon-runtime-axum-v0.4.4) - 2026-11-28

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.4.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.4.2...paigasus-helikon-runtime-axum-v0.4.3) - 2026-11-22

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.4.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.4.1...paigasus-helikon-runtime-axum-v0.4.2) - 2026-11-22

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.4.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.4.0...paigasus-helikon-runtime-axum-v0.4.1) - 2026-11-21

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.4.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.3.0...paigasus-helikon-runtime-axum-v0.4.0) - 2026-11-21

### Added

- *(core)* [**breaking**] SMA-830 let a tool declare a provider tool type ([#374](https://github.com/SMK1085/paigasus-helikon/pull/374))

### Fixed

- *(providers)* SMA-827 round trip gemini thought signatures through a provider data part ([#373](https://github.com/SMK1085/paigasus-helikon/pull/373))

### Upgrade notes

- Requires `paigasus-helikon-core` 0.8 (`ToolDef` is now `#[non_exhaustive]`; see the core CHANGELOG).

## [0.3.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.2.11...paigasus-helikon-runtime-axum-v0.3.0) - 2026-11-21

### Breaking changes

- This version replaces `0.2.11`. Version `0.2.11` requires `paigasus-helikon-core` 0.7, but it was released as a patch, so `cargo update` gave it to users who stay on core 0.6. Version `0.2.11` is yanked. The content of this version is the same as `0.2.11`.
- This version requires `paigasus-helikon-core` 0.7. If you stay on core 0.6, use `0.2.10`, the last version for core 0.6. A requirement cannot select a version below its minimum, so if your requirement is higher than `0.2.10` (for example a requirement that `cargo add` wrote for a yanked version), change it to `"0.2.10"` first, then run `cargo update`.
- If you already use core 0.7, change your requirement on `paigasus-helikon-runtime-axum` to `"0.3"`. If you do not, and your requirement allows the core 0.6 versions (for example `"0.2"`), `cargo update` moves you back to the core 0.6 build after the yank, and your build has two copies of core.

## [0.2.11](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.2.10...paigasus-helikon-runtime-axum-v0.2.11) - 2026-11-20

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.2.10](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.2.9...paigasus-helikon-runtime-axum-v0.2.10) - 2026-11-20

### Added

- *(core)* SMA-804 add a turn-boundary checkpoint to runcontext ([#346](https://github.com/SMK1085/paigasus-helikon/pull/346))
- *(core)* SMA-803 add deferred tool calls ([#344](https://github.com/SMK1085/paigasus-helikon/pull/344))

## [0.2.9](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.2.8...paigasus-helikon-runtime-axum-v0.2.9) - 2026-11-12

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.2.8](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.2.7...paigasus-helikon-runtime-axum-v0.2.8) - 2026-10-01

### Other

- update Cargo.toml dependencies

## [0.2.7](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.2.6...paigasus-helikon-runtime-axum-v0.2.7) - 2026-09-06

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.2.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.2.5...paigasus-helikon-runtime-axum-v0.2.6) - 2026-08-23

### Other

- *(workflows)* SMA-581 gate the published markdown surfaces with markdownlint-cli2 ([#220](https://github.com/SMK1085/paigasus-helikon/pull/220))

## [0.2.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.2.4...paigasus-helikon-runtime-axum-v0.2.5) - 2026-08-22

### Added

- *(core)* SMA-568 adopt paigasus:: tracing targets in core and the runtimes ([#216](https://github.com/SMK1085/paigasus-helikon/pull/216))

## [0.2.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.2.3...paigasus-helikon-runtime-axum-v0.2.4) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-runtime-tokio

## [0.2.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.2.2...paigasus-helikon-runtime-axum-v0.2.3) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.2.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.2.1...paigasus-helikon-runtime-axum-v0.2.2) - 2026-08-16

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

## [0.2.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.2.0...paigasus-helikon-runtime-axum-v0.2.1) - 2026-08-14

### Added

- *(core)* SMA-422 hoist the terminal-vs-cancel precedence resolver into core ([#193](https://github.com/SMK1085/paigasus-helikon/pull/193))

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.1.5...paigasus-helikon-runtime-axum-v0.2.0) - 2026-08-07

### Added

- *(runtime)* [**breaking**] SMA-482 harden 5xx redaction, session-principal binding, and run caps ([#177](https://github.com/SMK1085/paigasus-helikon/pull/177))

## [0.1.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.1.4...paigasus-helikon-runtime-axum-v0.1.5) - 2026-08-04

### Added

- *(runtime)* SMA-343 add actix-web REST/SSE/WebSocket agent server ([#173](https://github.com/SMK1085/paigasus-helikon/pull/173))

## [0.1.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.1.3...paigasus-helikon-runtime-axum-v0.1.4) - 2026-07-18

### Fixed

- *(runtime-agentcore)* SMA-456 finalize one-shot json runs on client disconnect ([#150](https://github.com/SMK1085/paigasus-helikon/pull/150))

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.1.2...paigasus-helikon-runtime-axum-v0.1.3) - 2026-07-13

### Other

- update Cargo.toml dependencies

## [0.1.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-runtime-axum-v0.1.1...paigasus-helikon-runtime-axum-v0.1.2) - 2026-07-07

### Other

- updated the following local packages: paigasus-helikon-core, paigasus-helikon-runtime-tokio

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
