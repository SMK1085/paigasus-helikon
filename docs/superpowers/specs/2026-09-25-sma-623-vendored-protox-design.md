# SMA-623 — Compile the Temporal protos with `protox` and retire `setup-protoc`

- **Ticket:** [SMA-623](https://linear.app/smaschek/issue/SMA-623)
- **Date:** 2026-09-25
- **Status:** Design approved in conversation. Revised after the adversarial
  spec challenge (section 10). This spec is for Gate 1 review.
- **Decision:** **Adopt** the `vendored-protox` feature, always on. Retire
  `setup-protoc` in **two phases** (section 5).

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
  prost-build does not read `PROTOC` when `skip_protoc_run` is set
  (`prost-build-0.14.4/src/config.rs:937`), and `tonic-prost-build` forwards
  the setting (`tonic-prost-build-0.14.6/src/lib.rs:817-819`).
- The feature chain is `temporalio-client/vendored-protox` →
  `temporalio-common/vendored-protox` → `temporalio-protos/vendored-protox`.
  `temporalio-sdk` 1.0.0 and `temporalio-sdk-core` 0.9.0 do **not** forward the
  feature. Thus the workspace must enable it on `temporalio-client` or
  `temporalio-common`. Cargo unifies features, so one edge enables it for the
  single `temporalio-protos` instance in the graph.
- `temporalio-common` 1.0.0 also has a `build.rs`. It does not call a
  compiler. It reads the `descriptors.bin` file that `temporalio-protos` writes
  and generates `PayloadVisitor` and payload-limit code from it.
- Well-known types: the Temporal crate ships its own copies of most
  `google/protobuf/*.proto` files, but **not** `field_mask.proto`, which
  `workflowservice/v1/request_response.proto` and `workflow/v1/message.proto`
  import. With `protoc`, the build took that file from `PROTOC_INCLUDE` (the
  `include/` tree of the protoc release). With `protox`, the build takes it
  from the built-in `GoogleFileResolver` of `protox`. M4 shows that the
  generated code is the same.
- Only `paigasus-helikon-runtime-temporal` and the facade (through its
  `runtime-temporal` feature, `crates/paigasus-helikon/Cargo.toml:24`) depend
  on the Temporal family. The six `temporalio-*` dependencies of
  `runtime-temporal` are not optional.
- `protox` 0.9.1 is the newest release and the version that the two crates
  require. It declares `rust-version = "1.74.0"`. Its license is
  `MIT OR Apache-2.0`.
- The 13 new packages (M12) are all **build dependencies**. They contain no
  TLS or crypto crate; the evidence is the list in M12, not M15, because M15
  uses `-e normal`, which excludes build dependencies.
- The highest `rust-version` among the new packages is 1.82
  (`prost-reflect` 0.16.5). This is below the workspace MSRV 1.94.
- `miette` and `miette-derive` are `Apache-2.0` only. `cargo deny` accepts
  this (M16).
- New code runs at build time on every downstream build of
  `runtime-temporal`: two `logos-derive` proc-macros, `miette-derive`, and the
  `build.rs` of `logos-codegen` 0.15.1.
- `protox` does not document its parity with `protoc`. Section 3 measures it.
- docs.rs built `paigasus-helikon-runtime-temporal` 0.5.0 with success
  (`status.json`: `"doc_status": true`). docs.rs is not affected.

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
| M4 | Generated code, `temporalio-protos` (63 `.rs` files, which include the pbjson `*.serde.rs` output) | **Byte-identical** to variant A (`diff -r` of the whole `out/` directory, `descriptors.bin` excluded). |
| M5 | Generated code, `prost-wkt-types` (5 modules) | **Byte-identical** to variant A. |
| M6 | Generated code, `temporalio-common` | `payload_limits_impl.rs` is byte-identical. `payload_visitor_impl.rs` has the same 4462 lines in a different order: 3797 lines differ in a raw diff, and 0 lines differ after `sort`. The cause is not `protox`: `temporalio-common`'s `build.rs` iterates a `HashSet<String>` (`build.rs:98`, `:342`), so the order of the `impl` blocks changes between any two builds, also between two `protoc` builds. `payload_limits_impl.rs` is stable because its code sorts the names first (`build.rs:973-974`). The sorted comparison is the correct evidence. |
| M7 | `descriptors.bin` | Differs (1,327,424 against 1,281,320 bytes for `temporalio-protos`). `protoc --decode_raw` shows that all differences are in `SourceCodeInfo` (field 9 of `FileDescriptorProto`). prost-build reads `SourceCodeInfo` to make doc comments (`code_generator.rs:112`, `ast.rs:21`). M4 shows that the doc comments it makes are identical today. |
| M8 | Variant B, `cargo test -p paigasus-helikon-runtime-temporal`, no `protoc` | 94 passed, 0 failed, 0 ignored (86 unit, 6 `temporal_live`, 2 doc). |
| M9 | Clean build time, `-p paigasus-helikon-runtime-temporal` | A 37.50 s, B 37.75 s (second pair: A 36.02 s, B 35.20 s). The difference is in the noise. |
| M10 | Build-script run time | `temporalio-protos` 3.74 s → 3.00 s; `prost-wkt-types` 2.80 s → 1.70 s. |
| M11 | New units, summed compile time (they build in parallel) | 8.99 s. |
| M12 | New `Cargo.lock` entries | 13: `beef` 0.5.2, `logos` 0.15.1 and 0.16.1, `logos-codegen` 0.15.1 and 0.16.1, `logos-derive` 0.15.1 and 0.16.1, `miette` 7.6.0, `miette-derive` 7.6.0, `prost-reflect` 0.16.5, `protox` 0.9.1, `protox-parse` 0.9.0, `unicode-width` 0.1.14. No existing version changes. |
| M13 | Extra target size, build mode | 37,419,438 bytes raw; **10,904,747 bytes after `zstd -3`**. |
| M14 | Extra target size, check mode (`clippy`, `docs`, `doc-coverage`) | 37,434,446 bytes raw; **10,868,193 bytes after `zstd -3`**. Build dependencies and proc-macros compile fully in check mode too. |
| M15 | `cargo tree -i ring --all-features -e normal --target all` | "nothing to print". This is the ticket's acceptance check. It does not see build dependencies (see section 2). |
| M16 | `cargo deny check` | "advisories ok, bans ok, licenses ok, sources ok". |
| M17 | `cargo audit --deny warnings` | Exit 0, no findings. |

### Cache budget estimate

The cache entries that compile the Temporal family and store `target/` are
`clippy` (shared with `docs`), five `test` legs (the `macos-latest`/`1.94` leg
does not cache) and `doc-coverage`: **7 entries**. `msrv` checks
`paigasus-helikon-core` only (`msrv.yml:62`). `temporal-it` uses
`cache-targets: false`. `build-no-default-features` and `sessions-it` do not
compile `runtime-temporal`. Thus 7 entries grow by about 10.4 MB compressed
each: **about 73 MB, or 0.7 % of the 10 GiB budget.** This is an estimate from
macOS artifacts. The Linux and Windows artifact sizes can be different.
Section 8 gives the check after the merge.

### What the measurements do not prove

- They ran on macOS only. The required `test` legs on Linux and Windows give
  that proof on the PR.
- The timings are single samples. They show that there is no large cost. They
  do not show a small gain.
- They prove parity for the protos that Temporal ships **today**. A later
  Temporal release can use a protobuf feature that `protox` does not support
  (for example, protobuf editions). `protox` then rejects the input with a
  build error. A later proto can also get a different doc comment, because
  `SourceCodeInfo` differs (M7). Section 7 covers both.
- They do not cover the release-plz semver check against the **published**
  0.5.0 baseline, which has no `vendored-protox`. Section 5 handles this.

## 4. Decision

Adopt `vendored-protox`, **always on**, for CI and for crates.io users.

- The pin becomes tracked: `protox` is a normal locked crate. release-plz's
  `cargo update` on each release PR and the `audit`/`deny` gates see it.
  Dependabot does **not** track it, because it is a transitive build
  dependency and `.github/dependabot.yml` has no `dependency-type: all`.
- CI removes one network download (from GitHub releases) from 8 of 9 call
  sites in phase 1, and from the ninth in phase 2.
- The workspace no longer needs a system tool to build. This also helps
  contributors and downstream users.
- The cost is approximately 0.7 % of the cache budget, 13 lockfile entries
  (all build dependencies), and no measurable build time.

We rejected a default-on crate feature on `paigasus-helikon-runtime-temporal`
(which would let users go back to a system `protoc`). It adds a feature flag,
a facade pairing, README tables, and a CI leg that still needs `protoc` to test
the off path. No user asked for that choice. Section 7 names the condition to
revisit it.

## 5. Phases

release-plz runs `cargo-semver-checks` by default (`release-plz.toml` has no
`semver_check` key). The check builds the **registry baseline**:
`paigasus-helikon-runtime-temporal` 0.5.0, and the facade 0.6.2 with its
`runtime-temporal` feature. The baseline manifest has no `vendored-protox`, so
its build calls `protoc`. Every earlier semver check had `protoc` from
`setup-protoc` (`release-plz.yml:33-39`). A PR never runs `release-plz.yml`, so
a failure appears only after the merge. We could not test locally whether
release-plz stops or continues when a semver check fails
(`cargo-semver-checks` is not installed). Thus we take the safe path:

- **Phase 1 (this PR, SMA-623):** enable the feature. Remove `setup-protoc`
  from the 8 sites that do not publish. Keep `setup-protoc` in
  `release-plz.yml` and keep `.github/actions/setup-protoc/`. Do **not** edit
  `release-plz.yml` at all.
- **Phase 2 (a follow-up Linear ticket, filed after Gate 1):** after
  release-plz has published a `runtime-temporal` version and a facade version
  that include `vendored-protox`, remove the `setup-protoc` step from
  `release-plz.yml`, delete `.github/actions/setup-protoc/`, and remove the
  pin from `CLAUDE.md` and the runbook. That PR uses a `ci(workflows)` title,
  as `CLAUDE.md` requires for release-infrastructure edits.

Phase 1 alone does not remove the four pins. It reduces them to one consumer.
The ticket acceptance "the four pins and the setup action removed" is complete
after phase 2.

## 6. Changes in phase 1

### 6.1 Manifest

In the root `Cargo.toml`, `[workspace.dependencies]`:

```toml
temporalio-client   = { version = "1.0", default-features = false, features = ["tls-aws-lc", "vendored-protox"] }
```

Put the new comment **inside** the existing Temporal comment block
(`Cargo.toml:115-140`). It must say:

- `vendored-protox` compiles the Temporal protos with `protox` (pure Rust), so
  no system `protoc` is necessary (SMA-623).
- The feature must stay on `temporalio-client` or `temporalio-common`, because
  `temporalio-sdk` and `temporalio-sdk-core` do not forward it.
- `protox` and its dependencies are **build** dependencies. The `ring` check in
  the same block (`Cargo.toml:128`) uses `-e normal` and does not cover them.
- Two CI guards (section 6.3) fail the required gates if the feature
  disappears.

Let cargo add the new entries to `Cargo.lock` with a normal `cargo build`
(not `--locked`). Do not run a bare `cargo update`. The lockfile diff must add
only packages that are reachable from `protox`, and it must not change the
version of any existing package. If a package in M12 has published a new
patch release, the diff can show a newer version than M12; that is acceptable.

### 6.2 CI: remove `setup-protoc` from 8 sites

Remove the `setup-protoc` step and its rationale comment at these sites:

| File | Job |
|---|---|
| `.github/workflows/ci.yml` | `clippy`, `test`, `build-no-default-features`, `docs`, `doc-coverage`, `sessions-it` |
| `.github/workflows/msrv.yml` | `verify` |
| `.github/workflows/integration.yml` | `temporal-it` |

Do not touch `.github/workflows/release-plz.yml` or
`.github/actions/setup-protoc/` (phase 2).

Keep the `'.github/actions/**'` entries in the path filters of `sessions-it`
(`ci.yml`) and `temporal-it` (`integration.yml`). Replace their comments with a
generic reason: the entry makes the job run when a local composite action
changes. A glob that matches no used file costs nothing, and the entry keeps
the protection that the old comment describes.

Change the `temporal-it` `timeout-minutes` comment in `integration.yml`
("prost/tonic + protoc codegen") to name `protox` codegen instead.

### 6.3 CI: regression guards

**Guard 1 — feature present (deterministic).** In the
`build-no-default-features` job of `ci.yml`, next to the existing `cargo tree`
assertion (`ci.yml:193-200`), add a step that fails if the feature is not
active:

```bash
cargo tree -p paigasus-helikon-runtime-temporal -e features -i prost-wkt-types \
  | grep -q 'feature "vendored-protox"'
```

The step must print a clear error that names SMA-623 when it fails. The
implementation must prove the step both ways: it passes with the feature, and
it fails when the feature is removed from a local copy of the manifest. This
guard does not depend on the cache or on the runner image.

**Guard 2 — no system protoc (catches any new protoc user).** In the
workflow-level `env:` of `ci.yml`, add:

```yaml
  # SMA-623: the workspace compiles the Temporal protos with protox
  # (temporalio-client's `vendored-protox` feature) and needs no system protoc.
  # This path does not exist. prost-build reads PROTOC before PATH, so a build
  # script that calls protoc fails with "Could not find `protoc`" instead of
  # silently using a protoc that a runner image happens to ship.
  PROTOC: /nonexistent/protoc-see-SMA-623
```

- Guard 2 also catches a **new** dependency whose build script calls `protoc`.
  Guard 1 cannot see that.
- Guard 2 fires only when a build script runs. The Temporal build scripts do
  not emit `rerun-if-env-changed=PROTOC`, so a cached build unit does not run
  again. This is why Guard 1 exists.
- The variable name has no `CARGO_` or `RUST` prefix. It is not part of the
  rust-cache key, and `scripts/check-cargo-profile-env-sync.sh:59` does not
  match it.
- M2 and M3 prove that a build with the feature passes when `PROTOC` points to
  a path that does not exist. On Windows, the path also does not exist, and
  any spawn failure fails the build (`config.rs:972-980`).
- Both guards go into `ci.yml` only. `msrv.yml` and `integration.yml` do not
  get them; the required gates in `ci.yml` block the merge.

### 6.4 Documents

| File | Change |
|---|---|
| `CONTRIBUTING.md` "Build prerequisites" (lines 148-160) | Replace the `protoc` requirement, the install commands, the `PROTOC` hint and the CI paragraph. New text: the workspace needs only the Rust toolchain. `paigasus-helikon-runtime-temporal` compiles the Temporal protos with `protox` through `temporalio-client`'s `vendored-protox` feature. No system `protoc` is necessary. |
| `CONTRIBUTING.md:250` ("like `PROTOC_VERSION`") | Correct it so it no longer names a general protoc pin. In phase 1 the pin still exists for `release-plz.yml` only; the text must say that, or use a different example. |
| `docs/runbooks/ci-architecture.md` "protoc" section (lines 101-105) | Rewrite it for phase 1: the workspace uses `protox`; `setup-protoc` stays only in `release-plz.yml` for the semver-check baseline, until phase 2; the two guards; the recovery in section 7. Keep the pin bump runbook, because the pin still exists until phase 2. |
| `docs/runbooks/ci-architecture.md:4` (scope line) | Add SMA-623. |
| `docs/runbooks/ci-architecture.md:113` (markdownlint section names `PROTOC_VERSION`) | Check the sentence and correct it if phase 1 makes it wrong. |
| `CLAUDE.md` (CI section) | Keep "Four pins" (the pin still exists), but say that `PROTOC_VERSION` and its digests serve only `release-plz.yml` now, and that phase 2 removes them. |
| `crates/paigasus-helikon-runtime-temporal/README.md` | Add one sentence: the crate needs no system `protoc`. It compiles the Temporal protos with `protox`. |
| `docs/book/src/` | No change. The book does not mention `protoc`. This is a conscious decision. |
| Earlier specs and plans (`docs/superpowers/**`) | No change. They are historical records. |

### 6.5 Version and release

- Do **not** bump any version by hand.
- The change alters the normalized manifest of
  `paigasus-helikon-runtime-temporal` (a new feature on a dependency). Thus
  release-plz is expected to release it with a patch bump (on 0.x, release-plz
  bumps `feat` as a patch) and to cascade the facade. This is correct:
  crates.io users get the `protox` build only through a new release, and
  phase 2 depends on that release.
- The PR title is
  `feat(runtime-temporal): SMA-623 build without a system protoc by compiling temporal protos with protox`.
  It becomes the CHANGELOG line, so it names the user effect, not the CI
  change. `runtime-temporal` is in the scope allowlist on `main`. Phase 1 does
  not edit `release-plz.yml` or `release-plz.toml`, so the `CLAUDE.md` rule for
  release-infrastructure commit types does not apply.
- After the merge, check the `release-plz` run and the `chore: release` PR and
  its CI.

## 7. Risks and recovery

| Risk | Effect | Mitigation |
|---|---|---|
| A later Temporal release uses a protobuf feature that `protox` does not support. | The build of `temporalio-protos` fails with a `protox` error. | The failure is loud. Recovery in this repo: remove `"vendored-protox"`, remove the two guards, and add `setup-protoc` again at the sites (from the git history of phase 1, or from the action itself before phase 2). The runbook names this path. |
| `protox` makes a different doc comment for a later proto (`SourceCodeInfo` differs, M7). | A different rustdoc text in generated types. | M4 proves identical comments today. The effect is limited to documentation text. We accept this risk. |
| `protox` makes different types for a later proto. | A silent difference in generated types. | Low probability: M4-M6 prove parity for the current protos, and `protox` builds a descriptor set with the same messages and fields. We accept this risk. |
| `temporalio-*` removes or renames the feature in a later release. | Cargo resolution fails ("does not have that feature"). | Dependabot can then fail to open the grouped bump PR; the error shows only in the Dependabot logs. release-plz's `cargo update` does not pick up a new major. The engineer who makes the Temporal bump sees the error. The bump runbook in the SMA-622 plan is the entry point. |
| Downstream users: a semver-compatible `temporalio-protos` 0.9.x breaks `protox`. | Every downstream build of `runtime-temporal` with a new lockfile fails. Users cannot turn the feature off (features are additive). | Workaround for users: `cargo update -p temporalio-protos --precise 0.9.0`. If this happens, revisit the rejected default-on crate feature (section 4). |
| release-plz semver check fails without `protoc`. | No release PR. | Phase 1 keeps `setup-protoc` in `release-plz.yml` (section 5). |
| Cache growth is larger on Linux or Windows than on macOS. | More use of the 10 GiB budget. | Check after the merge (section 8, step 14). The budget warns above 8.5 GiB. |

## 8. Verification

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
9. `grep -rniE 'protoc' .github CONTRIBUTING.md CLAUDE.md docs/runbooks README.md crates/*/README.md docs/book/src`
   shows only: the retained `release-plz.yml` step, `.github/actions/setup-protoc/`,
   the two guards, and the new document text. Review each hit.
10. The `Cargo.lock` diff only adds packages reachable from `protox` and
    changes no existing version. The PR body lists the added packages.
11. Guard 1 passes with the feature and fails when the feature is removed
    from a local copy of the manifest. Revert the local copy.

On the PR:

12. Every required context in `.github/rulesets/main-protection-checks.json`
    reports and is green, especially `test (windows-latest, stable)`,
    `test (macos-latest, stable)` and `build-no-default-features` (Guard 1).
13. `temporal-it` runs (the `Cargo.toml` change triggers its filter). It is a
    signal-only job. If it fails, read the log: a `protox` or `protoc` error is
    a real failure of this PR; a Temporal server timeout is the known flake,
    and one rerun is acceptable.

After the merge:

14. The `release-plz` run on `main` passes, and the `chore: release` PR's CI is
    green. Read the log, not only the conclusion.
15. Cache size: the lockfile change gives every entry a new key, and the first
    new entries restore from the old ones. Thus the total in
    `cache-budget.yml` mixes generations. Instead, compare the `size_in_bytes`
    of each job's new key with the same job's previous key (Actions cache API).
    Record the result as a comment on SMA-623.
16. File the phase 2 ticket in Linear (project `Paigasus Helikon`) before the
    PR opens, and link it from the PR body. Its trigger is: `runtime-temporal`
    and the facade are published with `vendored-protox`.

## 9. Acceptance (from the ticket)

- `CONTRIBUTING.md:150` is accurate: section 6.4.
- A recorded decision on `vendored-protox`: adopted (sections 3 and 4). The
  setup action and the four pins leave in phase 2 (section 5). The phase 2
  ticket tracks them, so they are no longer untracked by accident.
- `cargo tree -i ring --all-features -e normal --target all` prints "nothing
  to print": M15 and verification step 5.

## 10. Spec challenge changelog

The Opus spec-challenger returned **APPROVE WITH CHANGES**.

Folded in:

- **BLOCKER — the semver-check baseline needs `protoc`.** Accepted. The
  rollout is now two phases (section 5).
- **MAJOR — `feat` title against the release-infrastructure rule.** Resolved
  by the BLOCKER fix: phase 1 does not edit `release-plz.yml`.
- **MINOR — stale `PROTOC_VERSION` references** (`CONTRIBUTING.md:250`,
  `ci-architecture.md:113`) and a wider search in step 9. Accepted.
- **MINOR — the M6 explanation was wrong** (the cause is a `HashSet`, not
  `protox`). Corrected.
- **MINOR — M7 and risk row 2 said the descriptor sets are the same.**
  Corrected; the doc-comment risk has its own row.
- **MINOR — `field_mask.proto` is not vendored.** Corrected in section 2.
- **MINOR — the counts.** Corrected: 9 sites, 7 cache entries, about 73 MB.
- **MINOR — step 14 could not isolate the delta.** Replaced by a per-key
  comparison (step 15).
- **MINOR — removing `'.github/actions/**'` from the filters.** Accepted: the
  entries stay with a generic comment.
- **MINOR — Dependabot does not track `protox`.** Corrected in section 4 and
  in risk row 4.
- **MINOR — a deterministic guard.** Added as Guard 1. The `PROTOC` env guard
  stays as Guard 2, because it also catches a new `protoc` user.
- **MINOR — the PR title is CI text in the crate CHANGELOG.** Changed.
- **MINOR — step 10 was brittle.** Changed to "reachable from `protox`".
- **MINOR — no downstream risk row.** Added.
- **MINOR — M15 is not evidence about `protox`; missing facts** (MSRV 1.82,
  `miette` license, new build-time code, comment placement). Added to
  section 2 and section 6.1.

Questions answered:

- docs.rs built 0.5.0 with success; no change (section 2).
- The 63 files in M4 include the pbjson `*.serde.rs` output; the diff covered
  the whole `out/` directory.
- `temporal-it` failure handling: step 13.
- Whether release-plz stops on a semver-check error: not answered (no local
  `cargo-semver-checks`). The two-phase rollout makes the answer unnecessary.

Rejected: none.
