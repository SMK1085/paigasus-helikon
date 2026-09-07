# temporalio 1.0 Lockstep Bump — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move all six `temporalio-*` pins from 0.7 to the only coherent upstream set (1.0.0 × five + `temporalio-sdk-core` 0.9.0), fix the two API breaks, re-verify and re-cite every claim the old comments made against 0.7.0 sources, and release it as the breaking change it actually is.

**Architecture:** A dependency bump, not a refactor. The library compiles untouched apart from two forced changes (one test-only). The bulk of the work is truing-up eight `0.7.0` source citations, honouring the crate's own written "re-verify on any SDK bump" contract, and hand-bumping versions because `temporalio_client::Client` sits in this crate's public signature.

**Tech Stack:** Rust 2021, MSRV 1.94, cargo workspace, release-plz, Dependabot.

**Spec:** `docs/superpowers/specs/2026-09-07-temporalio-1.0-lockstep-bump-design.md`

## Global Constraints

- Target set is **forced, not chosen**: `temporalio-sdk` 1.0.0 declares `temporalio-sdk-core = "=0.9.0"`. Never move one pin alone.
- Preserve `default-features = false` on all six, and `features = ["tls-aws-lc"]` on `temporalio-client` and `temporalio-sdk-core`.
- `temporalio-sdk-core`'s `ephemeral-server` feature must stay unenabled.
- `cargo tree -i ring --all-features -e normal --target all` must print `warning: nothing to print.`
- Workspace MSRV stays `1.94` (`temporalio-sdk` 1.0.0 floor is 1.92.0, below it).
- Commit prefix: `<type>(<scope>): SMA-622 <message>`, subject lowercase after the ticket id.
- **`deps` is a valid scope, not a type.** The `.versionrc` type allowlist is `feat|fix|build|chore|ci|docs|style|refactor|perf|test|revert`; `deps`, `release`, `runtime-temporal`, `facade`, `spec`, `plan` are scopes. `deps(...)` is rejected by the `commit-msg` hook.
- `paigasus-helikon-core` is **not** bumped — this PR adds no core API.
- **Citation policy (new):** where a stable symbol exists, cite `file`, version, and the **symbol** (e.g. `impl Default for PayloadConverter`) rather than a bare line number. Line numbers have now broken twice across bumps. Keep a line number only where no unambiguous symbol anchor exists.

---

### Task 1: Move the six pins and fix the two forced API breaks

**Files:**
- Modify: `Cargo.toml` (the six `temporalio-*` entries)
- Modify: `Cargo.lock` (regenerated)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activity_input.rs` (test module)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/worker.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: a workspace that compiles and tests green against `temporalio-* = 1.0.0` / `temporalio-sdk-core = 0.9.0`. Every later task assumes this baseline.

- [ ] **Step 1: Set the six pins**

In `Cargo.toml`, the six entries become:

```toml
temporalio-sdk      = { version = "1.0", default-features = false }
temporalio-client   = { version = "1.0", default-features = false, features = ["tls-aws-lc"] }
temporalio-sdk-core = { version = "0.9", default-features = false, features = ["tls-aws-lc"] }
temporalio-common   = { version = "1.0", default-features = false }
temporalio-macros   = { version = "1.0", default-features = false }
temporalio-workflow = { version = "1.0", default-features = false }
```

- [ ] **Step 2: Regenerate the lockfile**

Run:
```bash
cargo update -p temporalio-sdk -p temporalio-client -p temporalio-sdk-core \
             -p temporalio-common -p temporalio-macros -p temporalio-workflow
```
Expected: exactly 8 family crates move, one copy each — `temporalio-{client,common,common-wasm,macros,sdk,workflow}` → 1.0.0, `temporalio-{protos,sdk-core}` → 0.9.0.

- [ ] **Step 3: Confirm the build fails in exactly the two expected places**

Run: `cargo build -p paigasus-helikon-runtime-temporal --all-targets`
Expected: FAIL with `E0639` (non-exhaustive struct) and `E0308` at `activity_input.rs`, plus one `deprecated` warning at `worker.rs`. Any *other* error means the upstream surface changed more than this plan assumes — stop and re-open the spec.

- [ ] **Step 4: Fix the test-only `SerializationContext` construction**

In `activity_input.rs`, extend the test-module import and replace the struct expression:

```rust
    use temporalio_common::data_converters::{
        ActivitySerializationContext, MultiArgs2, MultiArgs3, PayloadConverter,
        SerializationContextData,
    };
```

```rust
        // 1.0 made `SerializationContextData::Activity` a tuple variant and
        // `SerializationContext` `#[non_exhaustive]`, so this must go through the
        // constructors rather than a struct expression.
        let data = SerializationContextData::Activity(ActivitySerializationContext::new());
        let ctx = SerializationContext::new(&data, &converter);
```

- [ ] **Step 5: Migrate off the deprecated runtime constructor**

In `worker.rs`, `temporalio_sdk::Runtime::new_assume_tokio(runtime_options)` becomes `temporalio_sdk::Runtime::from_current_tokio(runtime_options)`, and the `// 0.7:` marker above the `RuntimeOptions` block becomes `// 1.0:`. This is required, not cosmetic — `clippy -D warnings` is a required gate and the old name is deprecated in 1.0.

- [ ] **Step 6: Verify the crate is green**

Run: `cargo clippy -p paigasus-helikon-runtime-temporal --all-features --all-targets -- -D warnings`
Expected: `Finished`, zero warnings.

Run: `cargo test -p paigasus-helikon-runtime-temporal --all-features`
Expected: 86 lib + 6 + 2 = 94 passed, 0 failed.

- [ ] **Step 7: Verify the TLS invariant across all targets**

Run: `cargo tree -i ring --all-features -e normal --target all`
Expected: `warning: nothing to print.`

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock crates/paigasus-helikon-runtime-temporal/src/
git commit -m "chore(deps): SMA-622 move the temporalio family to 1.0/0.9"
```

---

### Task 2: Correct the `Cargo.toml` comment block above the pins

**Files:**
- Modify: `Cargo.toml:32-34` (the `futures` note), `:115-126` (the TLS block), `:131-136` (`temporalio-macros`), `:137-143` (`temporalio-workflow`)

**Interfaces:**
- Consumes: Task 1's pins.
- Produces: comments whose cited versions match the lock and whose stated mechanism is true.

- [ ] **Step 1: Re-verify each claim before re-citing it**

Do not re-cite a claim you have not re-checked. Run:

```bash
R=~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f
grep -n '::futures::' $R/temporalio-macros-1.0.0/src/activities_definitions.rs
grep -n 'temporalio_macros' $R/temporalio-sdk-1.0.0/src/lib.rs | head -3
grep -c 'temporalio_workflow' $R/temporalio-macros-1.0.0/src/workflow_definitions.rs
```
Expected: `:670,673` for the futures paths; `lib.rs:14` importing `activities` from `temporalio_macros`; `78` occurrences. All three claims hold — only the cited version changes.

- [ ] **Step 2: Update the `futures` note**

`temporalio-macros-0.7.0/src/activities_definitions.rs` → `temporalio-macros-1.0.0/src/activities_definitions.rs`.

- [ ] **Step 3: Update the `temporalio-macros` note**

`temporalio-sdk-0.7.0/src/lib.rs` → `temporalio-sdk-1.0.0/src/lib.rs`.

- [ ] **Step 4: Update the `temporalio-workflow` note**

`temporalio-macros-0.7.0/src/workflow_definitions.rs` → `temporalio-macros-1.0.0/src/workflow_definitions.rs`.

- [ ] **Step 5: Correct the TLS block's false mechanism**

The current text claims `temporalio-common → opentelemetry-otlp` **unconditionally** pulls a second `reqwest` **0.12** via hyper-rustls. That is false, and was false at 0.7.0 too. Verify first:

```bash
grep -n 'otel = \|dep:opentelemetry-otlp\|opentelemetry-otlp?/tls-aws-lc' \
  ~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/temporalio-common-1.0.0/Cargo.toml
grep -A1 '^name = "reqwest"' Cargo.lock
```
Expected: `opentelemetry-otlp` is `optional`, gated on an `otel` feature that nothing here enables; `tls-aws-lc` uses only the weak `opentelemetry-otlp?/tls-aws-lc`; exactly one `reqwest 0.13.4`.

Replace the mechanism sentence with the real one: `temporalio-common` *can* pull a second `reqwest` + TLS stack via `opentelemetry-otlp`, but only behind its `otel` feature, which nothing in this workspace enables; `tls-aws-lc` references it weakly (`opentelemetry-otlp?/…`) so it does not activate it. The standing risk is a **future feature flip** enabling `otel`, which is what the `cargo tree` check guards. Keep the existing `-e normal` explanation (it excludes the dev-dep `rcgen`) and add that the check must be run with `--target all`, since `temporalio-client` 1.0.0 carries a non-optional `tokio-rustls` and an optional `rustls-native-certs`.

- [ ] **Step 6: Verify nothing broke**

Run: `cargo metadata --format-version 1 > /dev/null && cargo fmt --all -- --check`
Expected: both succeed (comment-only edits must not alter formatting).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml
git commit -m "docs(deps): SMA-622 re-cite temporalio comments against 1.0 and fix the tls rationale"
```

---

### Task 3: Rewrite `activity_input.rs`'s module doc against 1.0

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activity_input.rs:1-95`

**Interfaces:**
- Consumes: Task 1's pins.
- Produces: a module doc whose termination argument matches 1.0's actual dispatch.

**Why this task exists:** the doc justifies the whole hand-written codec by arguing the re-entrant call terminates via *"UseWrappers -> the struct's default `to_payloads` -> default `to_payload` -> WrongEncoding"*. That was true in 0.7, where `PayloadConverter::to_payload` **delegated** to `to_payloads` and `from_payload` to `from_payloads`. In 1.0 both are **primary implementations** that dispatch `T::to_payload` / `T::from_payload` directly. The behaviour is still correct, but the stated reason is now wrong — and no test can catch that.

- [ ] **Step 1: Verify the new dispatch and the preserved behaviour**

```bash
R=~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f
D=$R/temporalio-common-wasm-1.0.0/src/data_converters.rs
grep -n 'T::to_payloads\|T::from_payloads\|T::to_payload(val, context)\|T::from_payload(context, payload)' $D
grep -n 'converters: vec!\[Self::UseWrappers' $D
grep -n 'impl<T> TemporalSerializable for T\|impl<T> TemporalDeserializable for T' $D
grep -n 'from_payloads' $R/temporalio-sdk-1.0.0/src/activities.rs | head -2
```
Expected: `T::to_payloads` at `:604`, `T::from_payloads` at `:637`, the `Composite` default at `:250-254`, blanket impls at `:653` / `:661`, and `pc.from_payloads` at `temporalio-sdk-1.0.0/src/activities.rs:583`.

That last line is the important one: **the SDK still decodes activity inputs through `from_payloads`**, so the SMA-484 arity-rejection arms still fire. Record it in the doc rather than leaving it inferred from "it compiles".

- [ ] **Step 2: Re-cite the stable references by symbol**

Per the citation policy, prefer symbol anchors over line numbers:

- `temporalio-macros-0.7.0/src/activities_definitions.rs:265-278` → `temporalio-macros-1.0.0/src/activities_definitions.rs`, `fn multi_args_input_type` (still maps `0 => ()`, `1 => the parameter's own type`, `n => MultiArgs{n}`; verified unchanged).
- `temporalio-common-wasm-0.7.0/src/data_converters.rs:200-206` → `temporalio-common-wasm-1.0.0/src/data_converters.rs`, `impl Default for PayloadConverter`.
- The blanket impls `:603-610` / `:611-627` → `impl<T> TemporalSerializable for T` / `impl<T> TemporalDeserializable for T` in the 1.0.0 file.
- `activities_definitions.rs:548` (the `"{ImplType}::{method}"` name derivation, cited near `ACT_RENDER`) → same file at 1.0.0, `:548`, **verified unchanged** — so registered activity names are stable across the bump.

- [ ] **Step 3: Rewrite the termination argument**

Replace the "# Why the hand-written impls are reached at all" paragraph so it describes 1.0: `PayloadConverter::default()` is still `Composite([UseWrappers, serde_json()])`; the `Composite` arm still tries each sub-converter in order; `UseWrappers` still dispatches to the overridable `T::to_payloads` (`:604`) / `T::from_payloads` (`:637`) before the serde arm applies its hard `payloads.len() != 1` check (`:631`).

State the termination argument in 1.0's terms: `to_payload`/`from_payload` are now primary implementations rather than delegating to the plural forms, so the inner serde-derived struct's `to_payload` goes `UseWrappers` -> `T::to_payload` -> the **unchanged** trait default -> `WrongEncoding` -> falls through to `serde_json`. Note the trait defaults are unchanged from 0.7 (`1.0.0:329-345` vs `0.7.0:279-295`), which is why the outcome is identical despite the restructured dispatch.

- [ ] **Step 4: Verify**

Run: `cargo test -p paigasus-helikon-runtime-temporal --all-features`
Expected: 94 passed, 0 failed.

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-runtime-temporal --all-features --no-deps`
Expected: success (doc edits must not introduce broken intra-doc links).

- [ ] **Step 5: Commit**

```bash
git add crates/paigasus-helikon-runtime-temporal/src/activity_input.rs
git commit -m "docs(runtime-temporal): SMA-622 rewrite the codec dispatch note against 1.0"
```

---

### Task 4: Honour the crate's "re-verify on any SDK bump" contract

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/lib.rs:332-339`
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activities.rs:15` (and `:346` marker)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/worker.rs:295-296`

**Interfaces:**
- Consumes: Task 1's pins.
- Produces: an upgrade-discipline doc pinned to 1.0.0/0.9.0 with the re-verification recorded.

**Why this task exists:** `lib.rs` says verbatim *"This statement is pinned to `temporalio-* = 0.7.0` and must be re-verified on any SDK bump."* SMA-549 honoured it for 0.7. Shipping 1.0 while the doc claims a 0.7.0 pin publishes a false statement on docs.rs about the crate's highest-risk property.

- [ ] **Step 1: Re-verify the replay-determinism claim yourself**

```bash
R=~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f
diff <(sed -n '371,396p' $R/temporalio-sdk-core-0.7.0/src/worker/workflow/machines/activity_state_machine.rs) \
     <(sed -n '382,407p' $R/temporalio-sdk-core-0.9.0/src/worker/workflow/machines/activity_state_machine.rs)
```
Expected: no output (byte-identical). The `IdAndTypeDeterminismChecks` gate compares `act_id` and `act_type` only, never payloads — so activity input encoding remains a non-hazard for replay.

- [ ] **Step 2: Update the pin and record the re-verification**

In `lib.rs`, change the citation from `temporalio-sdk-core-0.7.0` to `temporalio-sdk-core-0.9.0`, change *"pinned to `temporalio-* = 0.7.0`"* to `1.0.0` / `sdk-core 0.9.0`, and add a re-verification note in the established style, e.g.:

```
//! (Re-verified for 1.0 in SMA-622: `on_activity_task_scheduled` in
//! `temporalio-sdk-core-0.9.0` is byte-identical to 0.7.0's and still compares
//! only `act_id`/`activity_id` and `act_type`/`activity_type`, never payloads.
//! Activity and workflow *type names* are also derived unchanged, so registered
//! names are stable across the bump.)
```

- [ ] **Step 3: Add the mixed-fleet row to the upgrade matrix**

The spec flags that no gate proves a 0.7-built and a 1.0-built worker can share one task queue. State what **is** established (input encoding is not a replay hazard; activity/workflow names are stable) and that beyond those, a mixed-version window is unproven — recommend draining or a blue-green task queue, consistent with the existing 0.5.0 guidance in `CHANGELOG.md`.

- [ ] **Step 4: Re-cite `activities.rs`**

Update the `0.7.0` citation at `activities.rs:15` and the `// 0.7:` marker at `:346` (`record_heartbeat` is async and fallible) to `1.0`. Verify the async/fallible claim still holds before re-citing:

```bash
grep -n 'fn record_heartbeat' -A4 ~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/temporalio-sdk-1.0.0/src/activities.rs
```

- [ ] **Step 5: Fix the stale `run()` doc**

`worker.rs:295-296` claims `run()` "sets `WorkerTaskTypes::all()`", contradicting `worker.rs:555-560` and 1.0's own derivation (`temporalio-sdk-1.0.0/src/lib.rs:718-723`, `enable_nexus: false`). Correct it to describe what the code does: workflow and activity task types, not nexus.

- [ ] **Step 6: Verify**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-runtime-temporal --all-features --no-deps`
Expected: success.

Run: `cargo test -p paigasus-helikon-runtime-temporal --all-features`
Expected: 94 passed.

- [ ] **Step 7: Commit**

```bash
git add crates/paigasus-helikon-runtime-temporal/src/
git commit -m "docs(runtime-temporal): SMA-622 re-verify replay determinism against sdk-core 0.9"
```

---

### Task 5: State the required `temporalio-*` major for consumers

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/README.md` (quickstart, near `:32` and `:66`)
- Modify: `crates/paigasus-helikon-runtime-temporal/src/lib.rs` (doctest preamble)

**Interfaces:**
- Consumes: Task 1's pins.
- Produces: docs that tell a consumer which `temporalio-client` major to `cargo add`.

**Why this task exists:** the four names in the README snippets (`Client`, `ClientOptions`, `Connection`, `ConnectionOptions`) all survive 1.0, so the code still compiles — but the snippets tell the reader to `use temporalio_client::…` in *their own* crate, which requires a matching major. Neither the README nor the crate docs say which. A reader following the quickstart with `temporalio-client = "0.7"` gets an opaque type mismatch. `CLAUDE.md`'s README rule covers the "usage example" and "install story".

- [ ] **Step 1: Add the requirement to the README quickstart**

Above the first `use temporalio_client::…` block, add a line naming the requirement — that this crate exposes `temporalio_client::Client` in its public API, so the consumer must depend on the same major:

```markdown
> **Requires `temporalio-client` 1.x.** This crate takes a connected
> `temporalio_client::Client` in its public API, so your crate must depend on the
> same major: `cargo add temporalio-client@1`.
```

Do **not** add a hardcoded patch version — install snippets stay drift-free per `CLAUDE.md`.

- [ ] **Step 2: Mirror it in the crate docs**

Add the same one-line requirement to the `lib.rs` doctest preamble so docs.rs readers see it too.

- [ ] **Step 3: Verify**

Run: `npx markdownlint-cli2`
Expected: clean.

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-runtime-temporal --all-features --no-deps`
Expected: success.

- [ ] **Step 4: Commit**

```bash
git add crates/paigasus-helikon-runtime-temporal/README.md crates/paigasus-helikon-runtime-temporal/src/lib.rs
git commit -m "docs(runtime-temporal): SMA-622 name the required temporalio-client major"
```

---

### Task 6: Group the `temporalio-*` family in Dependabot

**Files:**
- Modify: `.github/dependabot.yml` (the cargo `groups:` block)

**Interfaces:**
- Consumes: nothing.
- Produces: a Dependabot config that proposes the family as one PR.

**Why this task exists:** this is the **second** lockstep bump for the same cause (SMA-549 was 0.5 → 0.7), so recurrence is established. A group is the idiomatic mechanism and, unlike `ignore`, does not suppress security bumps.

- [ ] **Step 1: Add the group before the catch-alls**

Dependabot assigns a dependency to the **first** matching group, and `rust-major` / `rust-minor-patch` use `patterns: ["*"]`. The new group must therefore be declared *above* them:

```yaml
      # The temporalio-* crates are version-locked to each other: temporalio-sdk
      # declares an exact pin on temporalio-sdk-core (1.0.0 requires =0.9.0) and
      # tilde pins on the rest, and the macro-generated code references the
      # runtime crates by absolute path. A lone bump produces two versions of a
      # family crate in Cargo.lock and macro output compiled against the wrong
      # runtime. Grouping keeps them in one PR (SMA-622; SMA-549 was the same
      # failure at 0.5 -> 0.7). Must stay ABOVE rust-major/rust-minor-patch,
      # since Dependabot uses the first matching group.
      temporalio:
        patterns:
          - "temporalio*"
```

- [ ] **Step 2: Verify the YAML parses and the ordering is right**

```bash
python3 -c "import yaml,sys; d=yaml.safe_load(open('.github/dependabot.yml')); \
g=list(d['updates'][0]['groups']); print(g); \
assert g.index('temporalio') < g.index('rust-major'), 'temporalio group must precede rust-major'"
```
Expected: the group list prints with `temporalio` before `rust-major`, no assertion error.

- [ ] **Step 3: Commit**

```bash
git add .github/dependabot.yml
git commit -m "ci(deps): SMA-622 group the temporalio crates so they move in lockstep"
```

---

### Task 7: Hand-bump versions for the breaking public-API change

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/Cargo.toml` (version)
- Modify: `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md`
- Modify: `crates/paigasus-helikon/Cargo.toml` (version)
- Modify: `crates/paigasus-helikon/CHANGELOG.md`
- Modify: `Cargo.toml:162` (facade self-pin) and `:172` (sibling pin)

**Interfaces:**
- Consumes: every prior task.
- Produces: a release-ready version set.

**Why this task exists:** `temporalio_client::Client` is in this crate's public signature — `runner.rs:104` (`pub fn new(client: Client, …)`) and `worker.rs:317` (`pub fn client(mut self, client: temporalio_client::Client)`). A release-plz patch bump (`0.4.6 → 0.4.7`) is *compatible* within `0.4.x`, so downstreams would auto-upgrade into a compile break. SMA-549 did exactly that (`0.4.1 → 0.4.2`); this PR treats that as the defect.

- [ ] **Step 1: Bump the crate**

`crates/paigasus-helikon-runtime-temporal/Cargo.toml`: `version = "0.4.6"` → `version = "0.5.0"`.

- [ ] **Step 2: Bump the sibling pin**

Root `Cargo.toml:172`: `version = "0.4.6"` → `version = "0.5.0"`.

- [ ] **Step 3: Bump the facade and its self-pin**

`crates/paigasus-helikon/Cargo.toml`: `version = "0.5.23"` → `version = "0.6.0"`.
Root `Cargo.toml:162`: `version = "0.5.23"` → `version = "0.6.0"`.

The facade takes a minor (breaking, under 0.x rules) rather than a patch because it re-exports the crate at `crates/paigasus-helikon/src/lib.rs:57` behind `runtime-temporal`, so the break propagates through its public API. Per `CLAUDE.md`, both the facade version **and** its self-pin must be edited by hand: release-plz's `dependencies_update` cascade only runs when release-plz itself performs the sibling bump.

- [ ] **Step 4: Write the crate CHANGELOG entry**

Under `## [Unreleased]` in `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md`, add a `## [0.5.0]` section marking the break explicitly, covering: the family move to 1.0.0/0.9.0; that consumers must move to `temporalio-client` 1.x because `Client` is in the public API; that activity input encoding is still **not** a replay hazard (re-verified against `sdk-core` 0.9.0) and activity/workflow type names are stable; and the mixed-fleet guidance (drain or blue-green) from Task 4.

- [ ] **Step 5: Write the facade CHANGELOG entry**

Under `## [Unreleased]` in `crates/paigasus-helikon/CHANGELOG.md`, add a `## [0.6.0]` section noting the propagated break for `runtime-temporal` feature users.

- [ ] **Step 6: Verify the workspace still resolves**

Run: `cargo metadata --format-version 1 > /dev/null`
Expected: success — a mismatched self-pin fails here.

Run: `cargo build --workspace --all-features`
Expected: success.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/paigasus-helikon-runtime-temporal/Cargo.toml \
        crates/paigasus-helikon-runtime-temporal/CHANGELOG.md \
        crates/paigasus-helikon/Cargo.toml crates/paigasus-helikon/CHANGELOG.md
git commit -m "chore(release): SMA-622 bump runtime-temporal to 0.5.0 and the facade to 0.6.0"
```

---

### Task 8: Run the full CI gate set locally

**Files:** none modified (verification only).

**Interfaces:**
- Consumes: Tasks 1-7.
- Produces: evidence that every required gate passes before the PR opens.

- [ ] **Step 1: Run every gate**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
DOC_COVERAGE_THRESHOLD=80 NIGHTLY_CHANNEL=nightly-2026-05-01 bash scripts/check-doc-coverage.sh
npx markdownlint-cli2
bash scripts/check-markdownlint-config.sh
bash scripts/check-cargo-profile-env-sync.sh
bash scripts/check-cargo-profile-env-sync-selftest.sh
mdbook build docs/book
```
Expected: all succeed. `cargo test --workspace --all-features` should report 0 failures across ~40 suites.

- [ ] **Step 2: Re-confirm the TLS invariant one final time**

Run: `cargo tree -i ring --all-features -e normal --target all`
Expected: `warning: nothing to print.`

- [ ] **Step 3: Confirm no duplicate family crates survived**

```bash
grep -c '^name = "temporalio' Cargo.lock
grep -A1 '^name = "temporalio' Cargo.lock | grep version
```
Expected: 8 family crates, one version line each — six at `1.0.0`, `temporalio-protos` and `temporalio-sdk-core` at `0.9.0`.

- [ ] **Step 4: Confirm the convco baseline is clean**

```bash
convco check "$(git merge-base origin/main HEAD)..HEAD"
```
Expected: pass. The baseline **must** be a merge-base — `convco check A..B` silently walks all history when `A` is not an ancestor of `B`.

---

## Post-implementation note

`temporal-it` is the real acceptance evidence and runs only in CI. It is **signal-only** — not a required check — so the PR *can* merge with it red. It must be read by a human at GATE 2, and it proves less than it appears to: it starts a fresh dev server, so it never replays a history written by a 0.7 worker. If it fails on a wire/API error rather than the known `crash_resume_mid_tool_call` timing flake, bumping `TEMPORAL_CLI_VERSION` (currently `1.8.2`) and its `TEMPORAL_CLI_SHA256` — **verified upstream, never regenerated from the failing download** — becomes part of this PR.

**Deferred to a follow-up ticket:** `temporalio-protos` 0.9.0 adds an opt-in `vendored-protox` feature (off by default, so nothing changes here). It makes `CONTRIBUTING.md:150` ("`prost-build` has no vendored `protoc` fallback") stale, and it is a candidate to retire the hand-pinned `PROTOC_VERSION` + three digests that `CLAUDE.md` flags as untracked.
