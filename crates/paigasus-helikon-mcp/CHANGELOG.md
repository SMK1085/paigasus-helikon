# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.3.1...paigasus-helikon-mcp-v0.3.2) - 2026-11-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.3.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.3.0...paigasus-helikon-mcp-v0.3.1) - 2026-11-21

### Other

- updated the following local packages: paigasus-helikon-core

## [0.3.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.2.0...paigasus-helikon-mcp-v0.3.0) - 2026-11-21

### Added

- *(core)* [**breaking**] SMA-830 let a tool declare a provider tool type ([#374](https://github.com/SMK1085/paigasus-helikon/pull/374))

### Upgrade notes

- Requires `paigasus-helikon-core` 0.8 (`ToolDef` is now `#[non_exhaustive]`; see the core CHANGELOG).

## [0.2.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.26...paigasus-helikon-mcp-v0.2.0) - 2026-11-21

### Breaking changes

- This version replaces `0.1.26`. Version `0.1.26` requires `paigasus-helikon-core` 0.7, but it was released as a patch, so `cargo update` gave it to users who stay on core 0.6. Version `0.1.26` is yanked. The content of this version is the same as `0.1.26`.
- This version requires `paigasus-helikon-core` 0.7. If you stay on core 0.6, use `0.1.25`, the last version for core 0.6. A requirement cannot select a version below its minimum, so if your requirement is higher than `0.1.25` (for example a requirement that `cargo add` wrote for a yanked version), change it to `"0.1.25"` first, then run `cargo update`.
- If you already use core 0.7, change your requirement on `paigasus-helikon-mcp` to `"0.2"`. If you do not, and your requirement allows the core 0.6 versions (for example `"0.1"`), `cargo update` moves you back to the core 0.6 build after the yank, and your build has two copies of core.

## [0.1.26](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.25...paigasus-helikon-mcp-v0.1.26) - 2026-11-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.25](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.24...paigasus-helikon-mcp-v0.1.25) - 2026-11-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.24](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.23...paigasus-helikon-mcp-v0.1.24) - 2026-11-12

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.23](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.22...paigasus-helikon-mcp-v0.1.23) - 2026-09-17

### Other

- *(deps)* bump rmcp to 3.4.0, migrate to ServerConfig ([#265](https://github.com/SMK1085/paigasus-helikon/pull/265))

## [0.1.22](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.21...paigasus-helikon-mcp-v0.1.22) - 2026-09-06

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.21](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.20...paigasus-helikon-mcp-v0.1.21) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.20](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.19...paigasus-helikon-mcp-v0.1.20) - 2026-08-22

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.19](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.18...paigasus-helikon-mcp-v0.1.19) - 2026-08-16

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.18](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.17...paigasus-helikon-mcp-v0.1.18) - 2026-08-14

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.17](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.16...paigasus-helikon-mcp-v0.1.17) - 2026-08-04

### Other

- *(deps)* bump rmcp to 3.1 ([#170](https://github.com/SMK1085/paigasus-helikon/pull/170))

## [0.1.16](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.15...paigasus-helikon-mcp-v0.1.16) - 2026-07-18

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.15](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.14...paigasus-helikon-mcp-v0.1.15) - 2026-07-12

### Other

- *(deps)* bump rmcp 1.8.0 → 2.1.0 and migrate mcp to the ContentBlock API ([#144](https://github.com/SMK1085/paigasus-helikon/pull/144))

## [0.1.14](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.13...paigasus-helikon-mcp-v0.1.14) - 2026-07-07

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.13](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.12...paigasus-helikon-mcp-v0.1.13) - 2026-07-06

### Added

- *(mcp)* SMA-332 add `McpAgentServer::streamable_http_service_with`, taking an explicit rmcp `StreamableHttpServerConfig` — in particular stateless mode, needed by platforms such as AWS Bedrock AgentCore that inject their own `Mcp-Session-Id` header

## [0.1.12](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.11...paigasus-helikon-mcp-v0.1.12) - 2026-06-29

### Other

- updated the following local packages: paigasus-helikon-core

### Added

- *(mcp)* SMA-410 implement `ToolSource` for `McpServerHandle`

## [0.1.10](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.9...paigasus-helikon-mcp-v0.1.10) - 2026-06-21

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.9](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.8...paigasus-helikon-mcp-v0.1.9) - 2026-06-20

### Added

- *(core)* SMA-403 add runcontext ephemeral constructor and dependency setters ([#105](https://github.com/SMK1085/paigasus-helikon/pull/105))

## [0.1.8](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.7...paigasus-helikon-mcp-v0.1.8) - 2026-06-20

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.7](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.6...paigasus-helikon-mcp-v0.1.7) - 2026-06-18

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.6](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.5...paigasus-helikon-mcp-v0.1.6) - 2026-06-17

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.5](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.4...paigasus-helikon-mcp-v0.1.5) - 2026-06-16

### Other

- *(repo)* SMA-424 refresh crate READMEs to match the shipped surface ([#93](https://github.com/SMK1085/paigasus-helikon/pull/93))

## [0.1.4](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.3...paigasus-helikon-mcp-v0.1.4) - 2026-06-16

### Other

- *(repo)* SMA-423 refresh the book to match the shipped surface ([#91](https://github.com/SMK1085/paigasus-helikon/pull/91))

## [0.1.3](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.2...paigasus-helikon-mcp-v0.1.3) - 2026-06-16

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.2](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.1...paigasus-helikon-mcp-v0.1.2) - 2026-06-15

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.1](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.1.0...paigasus-helikon-mcp-v0.1.1) - 2026-06-14

### Other

- updated the following local packages: paigasus-helikon-core

## [0.1.0](https://github.com/SMK1085/paigasus-helikon/compare/paigasus-helikon-mcp-v0.0.0...paigasus-helikon-mcp-v0.1.0) - 2026-06-10

### Added

- *(mcp)* SMA-327 rmcp 1.7 MCP client (`McpServerHandle`): stdio / child-process
  / streamable-HTTP transports, tool adaptation with effect mapping, tool
  prefixing, lazy mode and `search_tools` for on-demand discovery.
- *(mcp)* SMA-327 rmcp 1.7 MCP server (`McpAgentServer`): expose any
  `Agent<Ctx>` as a single MCP tool, ctx factory, run timeout +
  cancel-on-disconnect, stdio and streamable-HTTP serving.

### Other

- *(release)* SMA-327 lift stage-1 gates for paigasus-helikon-mcp

## [0.0.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-mcp-v0.0.0) - 2026-05-17

### Added

- *(mcp,tools)* SMA-304 add mcp and tools stub crates

### Other

- SMA-307 automated versioning with release-plz ([#5](https://github.com/SMK1085/paigasus-helikon/pull/5))
- SMA-305 build, test, clippy, fmt + doc-coverage matrix ([#2](https://github.com/SMK1085/paigasus-helikon/pull/2))
