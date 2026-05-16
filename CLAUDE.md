# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

The Paigasus AI SDK (codename **Helikon**, after Mt Helicon where Pegasus's hoof struck the Hippocrene spring). A Rust SDK for building AI agents. All crates live under the `paigasus-helikon-*` namespace.

The full architectural reference lives in Notion: ["Crate Reference"](https://www.notion.so/355830e8fbaa813c92e8c1aa9985fd3f). Linear project: `Paigasus Helikon` (issues prefixed `SMA-`).

## Common commands

```bash
cargo build --workspace                              # all 13 crates
cargo build --workspace --all-features               # facade with every optional crate
cargo doc --workspace --all-features --no-deps       # docs (matches CI)
cargo run -p paigasus-helikon-cli --bin helikon
cargo run -p paigasus-helikon-cli --bin paigasus-helikon
```

To reproduce CI locally: `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`.

## Workspace layout

13 crates under `crates/`. The facade `paigasus-helikon` re-exports `paigasus-helikon-core` unconditionally and the other 10 sibling crates behind Cargo features. Stub crates print docstrings only — real implementations land in subsequent SMA-* tickets.

Workspace inheritance is **mandatory**: per-crate `Cargo.toml`s only set `name`, `description`, and any crate-specific bits. Everything else (`version`, `edition`, `rust-version`, `authors`, `license`, `repository`, `homepage`, `keywords`, `categories`) inherits from `[workspace.package]` in the root `Cargo.toml`. Don't hardcode these per-crate.

Third-party version pins live in `[workspace.dependencies]` (root). Members reference them via `dep.workspace = true`. Internal crate paths are also in `[workspace.dependencies]` so the facade can use `workspace = true` consistently.

## Non-obvious patterns to preserve

- **Feature naming**: kebab-case in `[features]` (`runtime-tokio`), snake-case in `pub use` aliases (`runtime_tokio`). They must stay paired across the facade's `Cargo.toml` and `src/lib.rs`.
- **`paigasus-helikon-cli` uses `autobins = false`** because the `paigasus-helikon` (hyphen) binary maps to `src/bin/paigasus_helikon.rs` (underscore — hyphens are illegal in Rust filenames). Removing `autobins = false` reintroduces a phantom `paigasus_helikon` binary that conflicts with the explicit `[[bin]]` entry.
- **`paigasus-helikon-macros` is a proc-macro crate from day one** (`[lib] proc-macro = true`). Don't convert it to a regular lib even though it currently has no macros.
- **The `paigasus-helikon` facade lib shares its name with the `paigasus-helikon` CLI binary by design** (Notion ref's "fully-qualified shim alias"). This produces a non-fatal `cargo doc` filename-collision warning. Don't "fix" it by renaming either — both names are user-facing API. The accepted future fix is `doc = false` on the CLI binary entry.
- **License is MIT only** (decided 2026-05-16). Don't add `LICENSE-APACHE` or set `license = "Apache-2.0 OR MIT"` even though the Cargo ecosystem convention is dual-licensing.
- **MSRV is `1.75`** (workspace-package level). If a dep raises MSRV, bump `rust-version` to what cargo demands rather than downgrading the dep.

## Workflow conventions

- Branch per Linear issue: `feature/<sma-####>-<kebab-title>`. The branch name is pre-computed in each Linear ticket's `gitBranchName` field.
- Design artifacts per ticket land under `docs/superpowers/specs/YYYY-MM-DD-<topic>-design.md` and `docs/superpowers/plans/YYYY-MM-DD-<topic>.md` on `main` before implementation starts.
- Commit prefix: `<type>(<scope>): SMA-### <message>` (e.g. `feat(facade): SMA-304 ...`).
- Don't auto-close Linear issues from PR merge — move status manually after review.

## CI

`.github/workflows/ci.yml` runs `cargo build --workspace` + `cargo doc --workspace --all-features --no-deps` with `RUSTDOCFLAGS=-D warnings` on push/PR. No `fmt`/`clippy` gate yet despite `rust-toolchain.toml` installing both — both are tracked follow-ups for the first real-Rust ticket.

## Cargo.lock

Committed (workspace contains a binary).
