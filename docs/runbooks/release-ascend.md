# Release & Stub-Ascend Reference

> **Scope:** the crate version lifecycle, the stub-ascend recipe, and the two
> publish deadlocks it has caused (SMA-321, SMA-346).
> This runbook is **not** linked from the public mdBook — it lives standalone under
> `docs/runbooks/` to avoid linkcheck coupling. It holds the *rationale and incident
> history*; the operative rules stay in `CLAUDE.md`.

## Version lifecycle

**Per-crate version is the one exception**, with a two-state lifecycle:

1. **Stub state — `version = "0.0.0"` + `publish = false` in Cargo.toml + `release = false` block in `release-plz.toml`.** Every stub was pre-published once to crates.io at `0.0.0` during SMA-385 to claim the name and satisfy the facade's optional-dep resolver. After that pre-publish, cargo refuses to republish (the per-crate `publish = false`); release-plz ignores them entirely (the `release = false`).
2. **Released state — bumped to a real version (≥ `0.1.0`)** after the first real public-API ticket lands. The 4-step ascend recipe:
   - Bump `version = "0.0.0"` → `"0.1.0"` in the crate's `Cargo.toml`.
   - Remove `publish = false` from that `Cargo.toml`.
   - Remove the crate's `[[package]] … release = false` block from `release-plz.toml`.
   - Land as one `chore(release): SMA-### lift stage-1 gates for <crate>` commit on the feature branch alongside the implementation. release-plz handles the first crates.io publish on CI.

   The 4-step recipe applies to **stubs ascending from `0.0.0`**. The ten already-released crates (`-core`, facade, `-macros`, `-providers-openai`, `-providers-anthropic`, `-providers-bedrock`, `-sessions-sqlite`, `-runtime-tokio`, `-mcp`, `-tools`) ship through release-plz's normal flow — no manual ritual needed for their future bumps. The historical chain of `chore(release): … escape release-plz 0.0.0 trap …` commits in the git log (SMA-317/347/350/372/382) is pre-Stage-1 archaeology and won't recur.

## Caveat 1 — same-PR core API needs a core bump (SMA-321)

**Caveat — when the ascending crate uses `paigasus-helikon-core` API added in the *same* PR, bump `core` too** (a 5th step). `cargo publish --verify` builds the ascending crate's tarball against the **registry** core (the `path` is stripped at publish), so if crates.io `core` lacks the new API the publish fails with `failed to verify package tarball`, and release-plz's combined job (SMA-351) aborts before its release-PR step — a deadlock, since `core` never gets its auto bump (a squashed `feat(<ascending-crate>)` commit attributes nothing to `core`). Fix: in the same PR, also bump `paigasus-helikon-core` (patch for additive/non-breaking-behind-`#[non_exhaustive]`, e.g. `0.2.0` → `0.2.1`) and its `[workspace.dependencies]` pin + CHANGELOG. release-plz then publishes `core` first, then the ascending crate verifies against the fresh `core` (dependency-ordered publish). Diagnosed in SMA-321: PR #45's release failed against the stale `core` `0.2.0`; PR #46 (`chore(release)` bumping `core` to `0.2.1`) cleared it.

## Caveat 2 — the manual core bump defeats the facade cascade (SMA-346)

**Second-order caveat — the manual core bump silently defeats `dependencies_update`, so the facade drifts.** `release-plz.toml` sets `dependencies_update = true`, which is *supposed* to cascade: when a sibling's version changes, release-plz bumps the facade's `[workspace.dependencies]` pin and gives the facade a patch bump. But that cascade only runs when **release-plz itself performs the sibling bump**. The same-PR manual bump above means the sibling version is already at target when the PR merges, so release-plz just publishes it and never runs the dependent-bump step — the facade (`paigasus-helikon`) is left untouched and stops tracking. Consequence: the facade stays at its old version with stale published dep reqs (e.g. after SMA-346, facade `0.2.0` still requested `paigasus-helikon-runtime-tokio = ^0.0.0`, so the new runner-boundary surface was unreachable through the facade's `runtime-tokio` feature). **Fix: in any PR that uses the same-PR manual bump, ALSO bump the facade** (patch: `version` in `crates/paigasus-helikon/Cargo.toml` + its `[workspace.dependencies]` self-pin + CHANGELOG) so it republishes with current sibling reqs. Diagnosed after SMA-346: PRs #48/#49 shipped `core 0.2.3` + `runtime-tokio 0.1.1` but left facade `0.2.0`; PR #50 (`chore(release)` bumping facade to `0.2.1`) cleared it. NB: feature branches must match the `branch-names` ruleset (`feature/**` or `hotfix/**`); a `chore/**` branch is rejected at push with `GH013 … creations being restricted`.

## Implementation status snapshot

**Implementation status** (as of 2026-07-06): every crate in the workspace carries a real implementation and publishes to crates.io — the original ten (`paigasus-helikon-core`, `paigasus-helikon`, `paigasus-helikon-macros`, `paigasus-helikon-providers-openai`, `paigasus-helikon-providers-anthropic`, `paigasus-helikon-providers-bedrock` (SMA-329), `paigasus-helikon-sessions-sqlite`, `paigasus-helikon-runtime-tokio` (ascended from stub in SMA-346), `paigasus-helikon-mcp` (SMA-327), and `paigasus-helikon-tools` (SMA-328)) plus `paigasus-helikon-providers-gemini` (SMA-449), `paigasus-helikon-sessions-postgres`/`-sessions-redis` (SMA-330), `paigasus-helikon-runtime-axum` (SMA-331), and the last four ascends — `paigasus-helikon-runtime-temporal`, `paigasus-helikon-runtime-agentcore` (both SMA-332), `paigasus-helikon-evals`, and `paigasus-helikon-cli` (both SMA-333). The workspace was first published to crates.io in SMA-385; zero name-claim stubs remain. `paigasus-helikon-cli` publishes as a **binary** crate (`0.1.0`) — its lib target is internal (`missing_docs` opted out) and carries no stability guarantee, publishing only so `cargo install paigasus-helikon-cli` resolves. `paigasus-helikon-sessions-testkit` is the sole non-published crate: an internal `Session` conformance test harness kept at `0.0.0` with `publish = false` by design, not a stub awaiting an ascend.
