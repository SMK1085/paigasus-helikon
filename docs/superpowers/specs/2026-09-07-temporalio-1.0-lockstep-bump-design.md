# SMA-622 — bump the temporalio crate family off 0.7 in lockstep

**Date:** 2026-09-07
**Status:** design (revised after adversarial challenge)
**Ticket:** [SMA-622](https://linear.app/smaschek/issue/SMA-622/bump-the-temporalio-crate-family-off-07-in-lockstep)

## Problem

Dependabot PR #247 proposed bumping `temporalio-macros` 0.7 → 0.8 **alone**, leaving the
other five `temporalio-*` pins at 0.7. That would have put two `temporalio-macros`
versions in `Cargo.lock` — 0.7.0 pulled transitively by `temporalio-sdk`/`-sdk-core`/
`-workflow`, and 0.8.0 as our direct dep — so `runtime-temporal` would invoke 0.8 macros
emitting code against a 0.7 runtime. The macro-generated code names runtime crates by
absolute path, so that skew is only accidentally compatible. We merged the `jsonschema`
half as #254 and closed #247; the macros bump is still outstanding.

## Target version: forced by upstream, not chosen

`temporalio-sdk` declares an **exact** pin on core
(`temporalio-sdk-1.0.0/Cargo.toml:264-281`):

| `temporalio-sdk` | requires `temporalio-sdk-core` |
| --- | --- |
| 0.8.0 | `=0.8.0` |
| 1.0.0 | `=0.9.0` |

plus `~1.0.0` on `-client`, `-common`, `-macros`, `-workflow`. The only coherent latest
set is **1.0.0 × five + `temporalio-sdk-core` 0.9.0**. `sdk-core` topping out at 0.9.0 is
not a gap — it is exactly what `temporalio-sdk` 1.0.0 asks for. Holding at 0.8 is
rejected: equally breaking, buys nothing, means doing this again.

Resolution adds no duplicate versions — one copy of each of the eight family crates:

```
temporalio-client 0.7.0 -> 1.0.0       temporalio-protos      0.7.0 -> 0.9.0
temporalio-common 0.7.0 -> 1.0.0       temporalio-sdk         0.7.0 -> 1.0.0
temporalio-common-wasm 0.7.0 -> 1.0.0  temporalio-sdk-core    0.7.0 -> 0.9.0
temporalio-macros 0.7.0 -> 1.0.0       temporalio-workflow    0.7.0 -> 1.0.0
```

`temporalio-sdk-core` is a direct dep that **no source file names** (`rg
temporalio_sdk_core crates/` is empty); it exists solely to force `tls-aws-lc` through
feature unification. Its pin is preserved deliberately — this note exists so a future
reader does not "clean it up".

**MSRV:** `temporalio-sdk` 0.7.0 declared no `rust-version`; 1.0.0 declares `1.92.0`
(`temporalio-sdk-1.0.0/Cargo.toml:14`), below the workspace floor of `1.94`. No MSRV
change, and the `1.94` matrix legs and `msrv.yml` stay green.

## Scope

### 1. The six pins

`default-features = false` throughout, `features = ["tls-aws-lc"]` on `temporalio-client`
/ `temporalio-sdk-core`, both preserved unchanged.

`temporalio-protos` 0.9.0 adds an opt-in `vendored-protox` feature
(`temporalio-protos-0.9.0/Cargo.toml:42-45`), off by default, so the system-`protoc`
requirement is unchanged. **Explicitly out of scope**, but it is a candidate to retire the
hand-pinned `PROTOC_VERSION` + three digests, and it makes `CONTRIBUTING.md:150`
("`prost-build` has no vendored `protoc` fallback") stale — flagged for a follow-up
ticket, not fixed here.

### 2. Source changes forced by the new majors

Exactly two compile-forced changes; the library is otherwise untouched.

**`src/activity_input.rs` (test-only).** 1.0 made `SerializationContextData::Activity` a
tuple variant carrying `ActivitySerializationContext`, and made `SerializationContext`
`#[non_exhaustive]` with a `new()` constructor. The `with_ctx` test helper used a struct
expression, now illegal outside the defining crate:

```rust
let data = SerializationContextData::Activity(ActivitySerializationContext::new());
let ctx = SerializationContext::new(&data, &converter);
```

Test-only — no production code constructs a `SerializationContext`.

**`src/worker.rs`.** `Runtime::new_assume_tokio` → `Runtime::from_current_tokio`. This is
**not** a pure rename, contrary to the first draft of this spec:

- 0.7 (`runtime.rs:95-101`) documented *"Panics if there is no currently active Tokio
  runtime"* and returned `Result<Self, anyhow::Error>`.
- 1.0 (`runtime.rs:183-188`) calls `Handle::try_current()` first and returns
  `Result<Self, RuntimeError>` with a new `NoCurrentTokioRuntime` variant.
- `Runtime` also lost `impl Deref<Target = CoreRuntime>` (0.7 `runtime.rs:119`).

`worker.rs:547-548` maps the error via `e.to_string()`, so `WorkerBuildError::Runtime`'s
message text changes and a former panic is now a returned error. No caller-visible
control-flow regression, because `build()` already returns `Result`. The old name still
compiles (it delegates) but is deprecated, and `clippy -D warnings` is a required gate —
so the change is forced, not cosmetic.

**Removed-API sweep (checked, none used here):** 1.0 removed `Worker::core_worker()`,
`Worker::set_detect_nondeterministic_futures()`, `Runtime: Deref<CoreRuntime>`,
`Runtime::new_assume_tokio_initialized_telem`, and `WorkflowStartOptions::start_signal`,
and moved `plugins` / `worker_interceptor` / `patch_activation_callback` /
`disable_payload_error_limit` behind a new `experimental` feature. This crate uses none of
them.

### 3. Payload encoding audit (new — the first draft did not do this)

`temporalio-common-wasm-1.0.0/src/data_converters/well_known.rs` is an entirely new file
with no 0.7.0 equivalent. Top-level `Vec<u8>` and `Option<Vec<u8>>` now encode as
`binary/plain` raw bytes (`well_known.rs:31-41`) instead of JSON.

"It compiles" cannot detect this, so each type crossing the wire was checked against
`WellKnownType::of`: `RenderInstructionsArgs`, `CallModelArgs`, `InvokeToolArgs`,
`String`, `ModelTurnResult`, `ToolCallOutcome`, `DurableRunOutcome`, and `WorkflowInput`
all resolve to `None` — unaffected. The single `Vec<u8>` on the wire is
`activities.rs:350`, `record_heartbeat(Vec::<u8>::new())`: an empty heartbeat payload that
is never read back. **Benign, but now stated rather than assumed.**

### 4. Replay determinism — the crate's own re-verification contract

`src/lib.rs:332-339` carries a standing obligation, verbatim: *"This statement is pinned
to `temporalio-* = 0.7.0` and **must be re-verified on any SDK bump**."* SMA-549 honoured
it for 0.7; the first draft of this spec missed it entirely, and `CHANGELOG.md:97` still
says "Verified against `temporalio-* = 0.5.0`".

**Re-verified for this bump.** `temporalio-sdk-core-0.9.0`'s
`activity_state_machine.rs:382-407` is byte-identical to 0.7.0's `:371-396`: the
`IdAndTypeDeterminismChecks` gate compares `act_id` and `act_type` only, never payloads.
The claim holds. `lib.rs` is updated to cite `1.0.0`/`0.9.0` and record this.

Workflow-type and activity-type name derivations are also unchanged
(`workflow_definitions.rs:644-647`, `activities_definitions.rs:548`), so **registered
names are stable across the bump** — the property that makes the replay guarantee useful.

### 5. Comment and doc maintenance — all eight citations, not three

Every `0.7.0` citation with a file or line number is updated. The first draft claimed
three and listed only the `Cargo.toml` ones.

Claims re-verified against 1.0.0 sources — **all still true**, only the citation moves:

| Site | Claim | Verified at 1.0.0 |
| --- | --- | --- |
| `Cargo.toml:34` | `#[activities]` output names `::futures::FutureExt` / `::futures::future::BoxFuture` absolutely | `activities_definitions.rs:670,673` |
| `Cargo.toml:132` | `temporalio-sdk` does **not** re-export `#[activities]`/`#[activity]`, so the direct dep stays required | `temporalio-sdk-1.0.0/src/lib.rs:14` still imports from `temporalio_macros` in its own doctest |
| `Cargo.toml:139` | workflow macro output references `::temporalio_workflow::…` | 78 occurrences in `workflow_definitions.rs` |
| `activity_input.rs:7` | arity/envelope reasoning | `activities_definitions.rs:265-278`, unchanged |
| `activity_input.rs:30` | context shape | moved `:200-206` → `:251-254` |
| `activity_input.rs:83` | converter dispatch | `:548`, unchanged |
| `activities.rs:15` | activity-context notes | re-cite |
| `lib.rs:334` | replay determinism (§4) | re-cite + record re-verification |

Line refs `:541`/`:576`/`:570-572`/`:603-627`/`:611-627` in `activity_input.rs:32-42`
become `:604`/`:637`/`:631`/`:653-660`/`:661-`.

**`activity_input.rs`'s module doc now describes dispatch that no longer exists.** It
justifies the hand-written codec by arguing the re-entrant call terminates: *"UseWrappers
-> the struct's default `to_payloads` -> default `to_payload` -> WrongEncoding."* True in
0.7, where `to_payload` delegated to `to_payloads`
(`temporalio-common-wasm-0.7.0/src/data_converters.rs:511`) and `from_payload` to
`from_payloads` (`:523`). In 1.0 both are **primary implementations** dispatching
`T::to_payload` / `T::from_payload` directly (`1.0.0:548`, `:577`). The doc is rewritten
against 1.0.

The behaviour is still correct — trait defaults are unchanged (`1.0.0:329-345` vs
`0.7.0:279-295`), and the SDK still decodes activity inputs via `pc.from_payloads`
(`temporalio-sdk-1.0.0/src/activities.rs:583`), so **the SMA-484 arity-rejection arms
still fire**. That evidence is recorded rather than inferred from "it compiles".

Also fixed, two lines from an edited hunk: `worker.rs:295-296` claims `run()` "sets
`WorkerTaskTypes::all()`", contradicting `worker.rs:555-560` and 1.0's derivation
(`temporalio-sdk-1.0.0/src/lib.rs:718-723`, `enable_nexus: false`).

### 6. TLS / CryptoProvider — invariant holds, stated rationale was false

The invariant is re-verified and **holds**, now across all targets rather than the host
only:

- `cargo tree -i ring --all-features -e normal --target all` → `warning: nothing to
  print.` (`-e normal` deliberately excludes the dev-dep `rcgen`.)
- One `rustls v0.23.43`, backed by `aws-lc-rs v1.18.1`.
- `temporalio-sdk-core`'s `ephemeral-server` stays unenabled.

`--target all` matters more than before: `temporalio-client` 1.0.0 newly carries a
non-optional `tokio-rustls` and an optional `rustls-native-certs`
(`temporalio-client-1.0.0/Cargo.toml:131-133,157-159`) — exactly the deps most likely to
be platform-gated.

**The comment's stated mechanism is wrong and is corrected in this PR.** `Cargo.toml:116-118`
says `temporalio-common → opentelemetry-otlp` **unconditionally** pulls a second `reqwest`
(**0.12** via hyper-rustls). In both 0.7.0 and 1.0.0, `opentelemetry-otlp` and `reqwest`
are `optional = true` behind the `otel` feature, `tls-aws-lc` only uses the *weak*
`opentelemetry-otlp?/tls-aws-lc`, and nothing in this workspace enables `otel`. Both
declare `reqwest = "0.13"`, and `Cargo.lock` holds exactly one `reqwest 0.13.4`. The
comment was already wrong before this bump; it is corrected because it sits directly above
the six pins being changed, and because a maintainer re-verifying against a fiction is
worse than one with no comment.

### 7. Version and release mechanics — OPEN DECISION, see GATE 1

`temporalio_client::Client` is in this crate's **public** signature — `runner.rs:104`
(`pub fn new(client: Client, …)`) and `worker.rs:317` (`pub fn client(mut self, client:
temporalio_client::Client)`). Moving the family 0.7 → 1.0 therefore breaks every
downstream that constructs a `Client` from `temporalio-client = "0.7"`. The facade
re-exports the crate (`crates/paigasus-helikon/src/lib.rs:57`) behind `runtime-temporal`,
so it carries the same break.

Treating this as a plain `chore(deps)` yields a release-plz **patch** bump 0.4.6 → 0.4.7,
which Cargo treats as compatible within `0.4.x` — downstream picks it up on `cargo update`
and fails to compile. This is what SMA-549 did (0.5 → 0.7 shipped as `0.4.1 → 0.4.2`,
`CHANGELOG.md:34-38`), so the precedent exists but is arguably itself the defect.

Resolved at GATE 1. See "Decision required" below.

## Out of scope — deliberate calls

- **No `docs/book/` edit.** No page under `docs/book/src/` names a `temporalio` version or
  API. Conscious skip under the `CLAUDE.md` book rule.
- **`dependabot.yml`** — see "Decision required"; the recurrence argument changed.

## README — one line added

The four names in `README.md:32,66` (`Client`, `ClientOptions`, `Connection`,
`ConnectionOptions`) all survive 1.0 unchanged, so the snippets still compile. But they
instruct the reader to `use temporalio_client::…` in *their own* crate, which means
`cargo add temporalio-client` at a **matching major** — and neither the README nor the
crate docs say which. After this PR it must be 1.0; a reader following the quickstart with
`temporalio-client = "0.7"` gets an opaque type mismatch. `CLAUDE.md`'s README rule covers
the "usage example" and "install story", so one line naming the supported major is added
to the README quickstart and the `lib.rs` doctest preamble.

## Verification — and what green does *not* prove

| Gate | Status |
| --- | --- |
| `cargo clippy --workspace --all-features --all-targets -- -D warnings` | clean, 0 warnings |
| `cargo test --workspace --all-features` | exit 0, 40 suites, 0 failures |
| `cargo test -p paigasus-helikon-runtime-temporal --all-features` | 86 lib + 6 + 2 = 94, 0 failures |
| `cargo tree -i ring --all-features -e normal --target all` | "nothing to print" |
| `temporal-it` (integration.yml) | signal-only; **must be read before merge** |

**Limits, stated explicitly:**

1. `temporal-it` is **not a required check** (`integration.yml:3-9`; absent from
   `main-protection-checks.json`), so this PR *can* merge with it red. It is the best
   evidence available, not a gate. A human must read it — that is GATE 2.
2. `temporal-it` starts a **fresh** dev server, so it only ever replays histories written
   by 1.0 code. It proves **nothing** about replaying a history written by a 0.7 worker,
   which is the actual fleet-upgrade risk and precisely what `lib.rs:324-339` is about.
3. `cargo test -p … --all-features` **loud-skips** `temporal_live` unless
   `TEMPORAL_TEST_SERVER` is set, so the local run covers zero live behaviour.
4. The CryptoProvider invariant is asserted by `paigasus-helikon-tools`' `forkd_tls` test,
   in a *different* package — covered by the full-workspace run above, not by the
   `-p runtime-temporal` run.

## Risks

- **Server compatibility is the one thing no local gate can prove.** `temporal-it` pins
  `TEMPORAL_CLI_VERSION: 1.8.2`. A client jumping three majors could require a newer
  server. If `temporal-it` goes red on a wire/API error rather than the known
  `crash_resume_mid_tool_call` timing flake, bumping `TEMPORAL_CLI_VERSION` — and its
  `TEMPORAL_CLI_SHA256`, **verified upstream, never regenerated from the failing
  download** — becomes part of this PR.
- **`temporal-it` is flake-prone by design.** A red run must be read, not assumed to be
  the known flake.
- **Mixed-fleet replay is unproven.** Whether a 0.7-built and a 1.0-built worker can
  safely share one task queue during a rolling deploy is not established by any gate here.
  §4 shows activity *input encoding* is not a replay hazard and activity/workflow *names*
  are stable, which covers the known hazards — but the crate's upgrade matrix
  (`lib.rs:349-378`) should gain a row, and a drain note if the answer is "no".
- **TLS to Temporal Cloud is untested.** The live tests use plaintext `http://`, and
  `temporalio-client` 1.0.0 adds `dynamic-tls = ["dep:rustls-native-certs"]`.

## Decisions taken at GATE 1 (2026-09-07)

### 1. Version: hand-bump, semver-correct

`temporalio_client::Client` is in the public signature, so this is a breaking change and
release-plz's patch bump would let downstreams auto-upgrade into a compile break. The
SMA-549 precedent (0.5 → 0.7 as `0.4.1 → 0.4.2`) is treated as the defect, not the model.

| File | Change |
| --- | --- |
| `crates/paigasus-helikon-runtime-temporal/Cargo.toml` | `0.4.6` → **`0.5.0`** |
| root `Cargo.toml:172` (sibling pin) | `version = "0.4.6"` → `"0.5.0"` |
| `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md` | `## [0.5.0]` with a `[**breaking**]` entry naming the required `temporalio-*` major |
| `crates/paigasus-helikon/Cargo.toml` (facade) | `0.5.23` → **`0.6.0`** |
| root `Cargo.toml:162` (facade self-pin) | `version = "0.5.23"` → `"0.6.0"` |
| `crates/paigasus-helikon/CHANGELOG.md` | `## [0.6.0]` entry |

The facade moves to `0.6.0` rather than a patch because it re-exports the crate
(`src/lib.rs:57`) behind `runtime-temporal`, so the break propagates through its public
API. Both the facade version **and** its self-pin are edited by hand, because
`CLAUDE.md` records that release-plz's `dependencies_update` cascade only runs when
release-plz itself performs the sibling bump — a manual bump otherwise leaves the facade
advertising stale dep reqs.

`paigasus-helikon-core` is **not** bumped: this PR adds no core API, so the
"bump core in the same PR" caveat does not apply.

### 2. `dependabot.yml`: add a `temporalio*` group

Recurrence is established, not hypothetical — this is the second lockstep bump for the
same cause (SMA-549 was 0.5 → 0.7). A dedicated group in the existing `groups:` block is
the idiomatic mechanism for a lockstep family and, unlike an `ignore` entry, does not
suppress security bumps. It must be declared **before** the catch-all `rust-major` /
`rust-minor-patch` patterns, since Dependabot assigns a dependency to the first matching
group.
