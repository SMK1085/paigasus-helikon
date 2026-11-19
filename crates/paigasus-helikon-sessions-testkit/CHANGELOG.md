# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/SMK1085/paigasus-helikon/releases/tag/paigasus-helikon-sessions-testkit-v0.1.0) - 2026-11-19

### Added

- *(sessions)* SMA-805 first crates.io release of the shared `Session` conformance suite
  - `run_all` runs every conformance test against a factory that returns a fresh, empty `Arc<dyn Session>`
  - `run_append_read`, `run_watermark_exclusive`, `run_projection` and `run_concurrent_writers` are public, so a backend can run a subset
