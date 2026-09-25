# SMA-623 — Compile the Temporal protos with `protox` and retire `setup-protoc`

- **Ticket:** [SMA-623](https://linear.app/smaschek/issue/SMA-623)
- **Date:** 2026-09-25
- **Status:** Design approved in conversation; this spec is for Gate 1 review.
- **Decision:** **Adopt** the `vendored-protox` feature, always on.

## 1. Problem

`temporalio-protos` 0.9.0 has an opt-in feature, `vendored-protox`. The feature
compiles the Temporal `.proto` files with `protox`, a protobuf compiler in pure
Rust, instead of a system `protoc`. The workspace does not enable the feature
today. Thus every job that compiles the workspace downloads a pinned `protoc`
through the repo-local action `.github/actions/setup-protoc`.

Two problems follow from this:

1. `CONTRIBUTING.md:150` says that `prost-build` "has no vendored `protoc`
   fallback". This is no longer accurate.
2. `PROTOC_VERSION` and its three SHA-256 digests in
   `.github/actions/setup-protoc/install.sh` are hand-bumped pins. No tool
   tracks them. `CLAUDE.md` lists them as one of four such pin sites.

The ticket accepts two outcomes: adopt the feature and remove the pins, or
reject it with a measurement. The measurements in section 3 support adoption.

## 2. Facts about the dependency graph

These facts come from the registry sources and from `Cargo.lock` on `main` at
`7602ae18`.

- Only **two** build scripts in the complete locked graph (698 packages, all
  features) call `protoc`:
  - `temporalio-protos` 0.9.0 `build.rs`, through `tonic-prost-build`.
  - `prost-wkt-types` 0.7.2 `build.rs`, through `prost-build`.
  The other 92 build scripts in the graph (95 packages have a `build.rs`; the
  third is `temporalio-common`, see below) do not call `protoc`, `prost-build`,
  `tonic-build` or `protox`. `opentelemetry-proto` 0.32.0 is in the graph, but
  it has no `build.rs`.
- `temporalio-protos` declares its `prost-types` dependency as
  `package = "prost-wkt-types"`. Its feature
  `vendored-protox = ["dep:protox", "prost-types/vendored-protox"]` therefore
  enables `vendored-protox` on `prost-wkt-types`. One feature switches **both**
  build scripts to `protox::compile(...)` and `skip_protoc_run()`.
- The feature chain is `temporalio-client/vendored-protox` →
  `temporalio-common/vendored-protox` → `temporalio-protos/vendored-protox`.
  `temporalio-sdk` 1.0.0 and `temporalio-sdk-core` 0.9.0 do **not** forward the
  feature. Thus the workspace must enable it on `temporalio-client` or
  `temporalio-common`. Cargo unifies features, so one edge enables it for the
  single `temporalio-protos` instance in the graph.
- `temporalio-common` 1.0.0 also has a `build.rs`. It does not call a
  compiler. It reads the `descriptors.bin` file that `temporalio-protos` writes
  and generates `PayloadVisitor` and payload-limit code from it.
- The Temporal protos include their own copies of the well-known types
  (`google/protobuf/*.proto`). The build does not use the `include/` tree of a
  `protoc` release.
- Only `paigasus-helikon-runtime-temporal` (and the facade through its
  `runtime-temporal` feature) depends on the Temporal family. Its six
  `temporalio-*` dependencies are not optional.
- `protox` 0.9.1 is the newest release and the version that the two crates
  require. It declares `rust-version = "1.74.0"` (the workspace MSRV is 1.94). Its
  license is `MIT OR Apache-2.0`. Its dependencies (`protox-parse`,
  `prost-reflect`, `miette`, `logos`, `bytes`, `thiserror`) contain no TLS or
  crypto crate.
- `protox` does not document its parity with `protoc`. Section 3 measures it.

## 3. Measurements

All measurements ran on the development host (macOS, arm64) with CI's profile
(`CARGO_PROFILE_DEV_DEBUG=line-tables-only`), from an empty target directory,
in a throwaway worktree of `main` at `7602ae18`. Variant A uses the system
`protoc` (`libprotoc 36.2`; CI pins 35.1). Variant B adds `"vendored-protox"` to
the `temporalio-client` features and sets `PROTOC=/nonexistent/protoc`. Every
timing is **one sample**.

| # | Check | Result |
|---|---|---|
| M1 | Control: variant A with `PROTOC=/nonexistent/protoc` | **Fails** in `prost-wkt-types` (`build.rs:48`) and in `temporalio-protos`: "Could not find `protoc`". |
| M2 | Variant B, `cargo build -p paigasus-helikon-runtime-temporal` | **Passes** with no `protoc`. |
| M3 | Variant B, `cargo build --workspace --all-features` | **Passes** with no `protoc` (46.80 s). |
| M4 | Generated code, `temporalio-protos` (63 `.rs` files) | **Byte-identical** to variant A. |
| M5 | Generated code, `prost-wkt-types` (5 modules) | **Byte-identical** to variant A. |
| M6 | Generated code, `temporalio-common` | `payload_limits_impl.rs` is byte-identical. `payload_visitor_impl.rs` has the same 4462 lines in a different order: 3797 lines differ in a raw diff, and 0 lines differ after `sort`. The order of the `impl` blocks follows the order of the descriptors. Rust does not give a meaning to the order of `impl` blocks. |
| M7 | `descriptors.bin` | Differs (1,327,424 against 1,281,320 bytes for `temporalio-protos`). `protoc --decode_raw` shows that all differences are in `SourceCodeInfo` (field 9 of `FileDescriptorProto`). The code generators do not read source information for the output that M4-M6 compare. |
| M8 | Variant B, `cargo test -p paigasus-helikon-runtime-temporal`, no `protoc` | 94 passed, 0 failed, 0 ignored (86 unit, 6 `temporal_live`, 2 doc). |
| M9 | Clean build time, `-p paigasus-helikon-runtime-temporal` | A 37.50 s, B 37.75 s (second pair: A 36.02 s, B 35.20 s). The difference is in the noise. |
| M10 | Build-script run time | `temporalio-protos` 3.74 s → 3.00 s; `prost-wkt-types` 2.80 s → 1.70 s. |
| M11 | New units, summed compile time (they build in parallel) | 8.99 s. |
| M12 | New `Cargo.lock` entries | 13: `beef` 0.5.2, `logos` 0.15.1 and 0.16.1, `logos-codegen` 0.15.1 and 0.16.1, `logos-derive` 0.15.1 and 0.16.1, `miette` 7.6.0, `miette-derive` 7.6.0, `prost-reflect` 0.16.5, `protox` 0.9.1, `protox-parse` 0.9.0, `unicode-width` 0.1.14. No existing version changes. |
| M13 | Extra target size, build mode | 37,419,438 bytes raw; **10,904,747 bytes after `zstd -3`**. |
| M14 | Extra target size, check mode (`clippy`, `docs`, `doc-coverage`) | 37,434,446 bytes raw; **10,868,193 bytes after `zstd -3`**. Build dependencies and proc-macros compile fully in check mode too. |
| M15 | `cargo tree -i ring --all-features -e normal --target all` | "nothing to print". |
| M16 | `cargo deny check` | "advisories ok, bans ok, licenses ok, sources ok". |
| M17 | `cargo audit --deny warnings` | Exit 0, no findings. |

### Cache budget estimate

The cache entries that compile the Temporal family and store `target/` are
`clippy` (shared with `docs`), five `test` legs (the `macos-latest`/`1.94` leg
does not cache), `doc-coverage` and `msrv`. `temporal-it` uses
`cache-targets: false`. `build-no-default-features` and `sessions-it` do not
compile `runtime-temporal`. Thus about 8 entries grow by about 10.4 MB
compressed each: **about 85 MB, or 0.8 % of the 10 GiB budget.** This is an
estimate from macOS artifacts. The Linux and Windows artifact sizes can be
different. Section 7 gives the check after the merge.

### What the measurements do not prove

- They ran on macOS only. The required `test` legs on Linux and Windows give
  that proof on the PR.
- The timings are single samples. They show that there is no large cost. They
  do not show a small gain.
- They prove parity for the protos that Temporal ships **today**. A later
  Temporal release can use a protobuf feature that `protox` does not support
  (for example, protobuf editions). That failure is a build error, not a silent
  difference, because `protox` rejects input that it cannot compile. Section 6
  gives the recovery.

## 4. Decision

Adopt `vendored-protox`, **always on**, for CI and for crates.io users.

- The pins stop being untracked: `protox` is a normal locked crate, and
  Dependabot and release-plz's `cargo update` track it like every other
  dependency.
- CI removes one network download (from GitHub releases) from 8 call sites.
- The workspace no longer needs a system tool to build. This also helps
  contributors and downstream users.
- The cost is approximately 0.8 % of the cache budget, 13 lockfile entries, and
  no measurable build time.

We rejected a default-on crate feature on `paigasus-helikon-runtime-temporal`
(which would let users go back to a system `protoc`). It adds a feature flag,
a facade pairing, README tables, and a CI leg that still needs `protoc` to test
the off path. No user asked for that choice.

## 5. Changes

### 5.1 Manifest

In the root `Cargo.toml`, `[workspace.dependencies]`:

```toml
temporalio-client   = { version = "1.0", default-features = false, features = ["tls-aws-lc", "vendored-protox"] }
```

Add a comment above the line. It must say:

- `vendored-protox` compiles the Temporal protos with `protox` (pure Rust), so
  no system `protoc` is necessary (SMA-623).
- The feature must stay on `temporalio-client` or `temporalio-common`, because
  `temporalio-sdk` and `temporalio-sdk-core` do not forward it.
- CI's `PROTOC` guard (section 5.3) fails the required gates if the feature
  disappears.

Let cargo add the new entries to `Cargo.lock` with a normal `cargo build`
(not `--locked`). Do not run a bare `cargo update`. The lockfile diff must add
only the 13 entries in M12 and must not change the version of any existing
package.

### 5.2 CI: remove `setup-protoc`

Remove the `setup-protoc` step and its rationale comment at all 8 sites:

| File | Job |
|---|---|
| `.github/workflows/ci.yml` | `clippy`, `test`, `build-no-default-features`, `docs`, `doc-coverage`, `sessions-it` |
| `.github/workflows/msrv.yml` | `verify` |
| `.github/workflows/release-plz.yml` | `release-plz` |
| `.github/workflows/integration.yml` | `temporal-it` |

Delete `.github/actions/setup-protoc/` (`action.yml`, `install.sh`,
`verify.sh`, `selftest.sh`). It is the only directory in `.github/actions/`.

Remove the `'.github/actions/**'` entries and their comments from the path
filters of `sessions-it` (`ci.yml`) and `temporal-it` (`integration.yml`). The
comments say that these entries exist only for the protoc install. No other
local action exists. If a later change adds a local action that these jobs
use, that change must add the filter entry again.

Change the `temporal-it` `timeout-minutes` comment in `integration.yml`
("prost/tonic + protoc codegen") to name `protox` codegen instead.

### 5.3 CI: regression guard

In the workflow-level `env:` of `.github/workflows/ci.yml`, add:

```yaml
  # SMA-623: the workspace compiles the Temporal protos with protox
  # (temporalio-client's `vendored-protox` feature) and needs no system protoc.
  # This path does not exist. prost-build reads PROTOC before PATH, so if the
  # feature disappears, every required gate that compiles runtime-temporal
  # fails with "Could not find `protoc`" instead of silently using a protoc
  # that a runner image happens to ship.
  PROTOC: /nonexistent/protoc-see-SMA-623
```

- The variable name has no `CARGO_` or `RUST` prefix. Thus it is not part of
  the rust-cache key, and `scripts/check-cargo-profile-env-sync.sh` does not
  apply to it. The implementation must confirm this by reading the script.
- M2 and M3 prove that a build with the feature passes when `PROTOC` points to a
  path that does not exist.
- The guard goes into `ci.yml` only. The required gates are there, so a PR that
  removes the feature cannot merge. `msrv.yml`, `release-plz.yml` and
  `integration.yml` do not get the guard: `release-plz` publishes, and a
  failure there is more expensive than useful.
- Windows: the path is not a Windows path, but it also does not exist on
  Windows, so the result is the same. The Windows `test` leg on the PR
  confirms this.

### 5.4 Documents

| File | Change |
|---|---|
| `CONTRIBUTING.md` "Build prerequisites" (lines 148-160) | Replace the `protoc` requirement, the install commands, the `PROTOC` hint and the CI paragraph. New text: the workspace needs only the Rust toolchain. `paigasus-helikon-runtime-temporal` compiles the Temporal protos with `protox` through `temporalio-client`'s `vendored-protox` feature. No system `protoc` is necessary, and a `PROTOC` variable has no effect on the build. |
| `docs/runbooks/ci-architecture.md` "protoc" section (lines 101-105) | Replace it with a short section "protoc (retired)": what SMA-458 built, why SMA-623 removed it (section 4 in short form, with the measurement summary), the `PROTOC` guard, and the recovery path in section 6. |
| `CLAUDE.md` (CI section) | Change "Four pins are hand-bumped" to three pins. Remove the `PROTOC_VERSION` item. Keep the checksum sentence, because `TEMPORAL_CLI_SHA256` still uses it. Remove "the protoc bump runbook" from the pointer sentence to `ci-architecture.md`. |
| `crates/paigasus-helikon-runtime-temporal/README.md` | Add one sentence: the crate needs no system `protoc`. It compiles the Temporal protos with `protox`. |
| `docs/book/src/` | No change. The book does not mention `protoc`. This is a conscious decision. |
| Earlier specs and plans (`docs/superpowers/**`) | No change. They are historical records. |

### 5.5 Version and release

- Do **not** bump any version by hand.
- The change alters the normalized manifest of
  `paigasus-helikon-runtime-temporal` (a new feature on a dependency). Thus
  release-plz is expected to release it with a patch bump and to cascade the
  facade. This is correct: crates.io users get the `protox` build only through
  a new release.
- The PR title is
  `feat(runtime-temporal): SMA-623 compile temporal protos with protox and retire setup-protoc`.
  `runtime-temporal` is in the scope allowlist on `main`. `feat` puts the change
  in the CHANGELOG, where users read that `protoc` is no longer necessary.
- After the merge, check the `chore: release` PR and its CI.

## 6. Risks and recovery

| Risk | Effect | Mitigation |
|---|---|---|
| A later Temporal release uses a protobuf feature that `protox` does not support. | The build of `temporalio-protos` fails with a `protox` error. | The failure is loud. Recovery: restore `.github/actions/setup-protoc/` and its call sites from the SMA-623 merge commit's parent, remove `"vendored-protox"`, and remove the `PROTOC` guard. The history note in `ci-architecture.md` names this path. |
| `protox` generates different code for a later proto change. | A silent difference in generated types. | Low probability: `protox` builds the same `FileDescriptorSet` that `prost-build` consumes. M4-M6 prove parity for the current protos. We accept this risk. |
| `temporalio-*` removes or renames the feature in a later release. | The build fails, or it falls back to `protoc` and the `PROTOC` guard fails it. | The failure is loud in both cases. The bump PR sees it. |
| Cache growth is larger on Linux or Windows than on macOS. | More use of the 10 GiB budget. | Check `cache-budget.yml` after the merge (section 7). The budget warns above 8.5 GiB. |
| `release-plz` fails `cargo publish --verify` without `protoc`. | A red release run. | M3 builds the whole workspace without `protoc`. The `release-plz` run after the merge confirms it. The memory note on partial publishes applies: read the log, not the conclusion. |

## 7. Verification

Before the push:

1. `PROTOC=/nonexistent/protoc cargo build --workspace --all-features` passes.
2. `PROTOC=/nonexistent/protoc cargo test --workspace --all-features` passes.
   Run it in a worktree under the scratchpad if the macOS `NATIVE_ROOTS`
   failures in `providers-bedrock` appear.
3. `cargo fmt --all -- --check` and
   `cargo clippy --workspace --all-features --all-targets -- -D warnings` pass
   with `PROTOC=/nonexistent/protoc`.
4. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`
   passes with `PROTOC=/nonexistent/protoc`.
5. `cargo tree -i ring --all-features -e normal --target all` prints
   "nothing to print".
6. `cargo deny check` and `cargo audit --deny warnings` pass.
7. `bash scripts/check-cargo-profile-env-sync.sh` and its self-test pass.
8. `npx markdownlint-cli2` passes.
9. `grep -rn "setup-protoc" .github` finds nothing. A search for `protoc` in
   `.github/`, `CONTRIBUTING.md` and `CLAUDE.md` finds only the new text.
10. The `Cargo.lock` diff adds exactly the 13 entries in M12 and changes no
    existing version.

On the PR:

11. Every required context in `.github/rulesets/main-protection-checks.json`
    reports and is green, especially `test (windows-latest, stable)` and
    `test (macos-latest, stable)`.
12. `temporal-it` runs (the `Cargo.toml` change triggers its filter) and passes.

After the merge:

13. The `release-plz` run on `main` passes, and the `chore: release` PR's CI is
    green.
14. After `main` has saved new cache entries, compare the total in the next
    `cache-budget.yml` report with the last report before the merge. Record
    the delta as a comment on SMA-623.

## 8. Acceptance (from the ticket)

- `CONTRIBUTING.md:150` is accurate: section 5.4.
- A recorded decision on `vendored-protox`: adopted (sections 3 and 4), with
  the four pins and the setup action removed (section 5.2).
- `cargo tree -i ring --all-features -e normal --target all` prints "nothing
  to print": M15 and verification step 5.

## 9. Out of scope

- The other three hand-bumped pins (`TEMPORAL_CLI_VERSION`/`TEMPORAL_CLI_SHA256`,
  `NIGHTLY_TOOLCHAIN`, `markdownlint-cli2`).
- Any change to `temporalio-*` versions.
- A `protoc` guard in `msrv.yml`, `release-plz.yml` or `integration.yml`.
