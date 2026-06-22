# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
