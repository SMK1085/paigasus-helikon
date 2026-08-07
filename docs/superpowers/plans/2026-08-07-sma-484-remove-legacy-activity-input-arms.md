# SMA-484 — Remove the 0.2.x Legacy Activity-Input Decode Arms — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `paigasus-helikon-runtime-temporal` decode **only** the single-payload activity-input envelope, refusing the pre-envelope positional shapes with a named, actionable, payload-free diagnostic.

**Architecture:** `crates/paigasus-helikon-runtime-temporal/src/activity_input.rs` currently decodes both an envelope (1 payload) and legacy positional shapes (2 payloads for `render_instructions`/`call_model`, 3 for `invoke_tool`). The legacy arms are replaced by arms that decode nothing and return an `EncodingError` naming the activity, the payload count, and the recovery version. The `warn_legacy` helper becomes `reject_legacy`, which also emits `tracing::error!`. Encode is untouched. The remaining work is documentation: the crate's upgrade-discipline story inverts, and the wire break must be visible through the facade.

**Tech Stack:** Rust 2024, `temporalio-* = 0.5.0`, `serde` / `serde_json`, `tracing`. Tests are in-file `#[cfg(test)] mod tests`.

**Spec:** `docs/superpowers/specs/2026-08-07-sma-484-remove-legacy-activity-input-arms-design.md` — read it before starting. Section references below (§4.1, §5.2, §7.1 …) point into it.

## Global Constraints

- **Working directory:** `/Users/smaschek/dev/paigasus/paigasus-helikon/.claude/worktrees/sma-484` — a git worktree. Use paths relative to it, or absolute paths under it. Do **not** `cd` to the main checkout, and do **not** run `git checkout`, `git switch`, `git stash`, or anything else that moves `HEAD` or branch refs; the object store is shared with other sessions.
- **Branch:** `feature/sma-484-runtime-temporal-remove-the-02x-legacy-activity-input-decode`. Already created. Never switch off it.
- **Never `git add -A` or `git add .`** — `.env` and `.claude` are untracked but *not* gitignored in this repo. Stage explicit paths only, and verify with `git show --stat` after committing.
- **Run `cargo fmt --all` before every commit.** The `pre-commit` hook is a deliberate no-op; formatting is only caught at push time or in CI.
- **`missing_docs` is `warn` workspace-wide and CI runs `-D warnings`.** Every new item, including private helpers and struct fields, needs a `///` doc comment.
- **Never intra-doc-link (`[`crate::foo`]`) from a `pub` item's docs to a `pub(crate)`/private item** — `rustdoc::private_intra_doc_links` fails the required `docs` job while build and tests stay green. Everything in `activity_input.rs` is `pub(crate)`, so links *within* that module are fine; links from `lib.rs`'s public docs into it are not.
- **MSRV is 1.94.** Do not use newer language features.
- **Diagnostic messages must never contain payload bytes.** Activity-input decode errors land in Temporal history, which is a persistence boundary readable by anyone with namespace read. Only the activity name and the payload *count* may appear.
- **Version strings used verbatim across this plan** — and the two scopes are *not* interchangeable:
  - In **diagnostics** (the decode error, the module docs describing what is rejected), the removed shapes are **"0.2.1 and earlier"**, never "0.2.0/0.2.1" — `call_model`'s 2-payload shape is unchanged since 0.1.x, so the narrower range would mislabel a real case. See spec §9.
  - In **migration guidance** (the upgrade notes, the compatibility matrix, the README), the bridge scope is **"0.2.0 or 0.2.1"**. 0.2.2 decodes those two releases' arities specifically; 0.1.x cannot hop through it and must drain outright.
  - The recovery version is **`0.2.2`**. The release being built is **`0.3.0`**.
- **Do not edit any `Cargo.toml` `version` field.** release-plz performs the `0.2.2` → `0.3.0` bump from the PR title's `feat(runtime-temporal)!` type. Hand-bumping would break the release flow.
- **Verification command for every task:** `cargo test -p paigasus-helikon-runtime-temporal` for fast cycles. Before the final commit of the branch, run the full gate: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-features --all-targets -- -D warnings`, `cargo test --workspace --all-features`.
- **Work synchronously.** Run `cargo` commands in the foreground and wait for them. Do not background a build or test run and end your turn before it reaches a terminal status.

---

## File Structure

| file | responsibility | task |
|---|---|---|
| `crates/paigasus-helikon-runtime-temporal/src/activity_input.rs` | the codec; `reject_legacy`, the three `from_payloads` impls, and all unit tests | 1, 2 |
| `crates/paigasus-helikon-runtime-temporal/src/lib.rs` | crate-level § "Upgrade Discipline and Determinism"; the `mod activity_input` doc | 3 |
| `crates/paigasus-helikon-runtime-temporal/README.md` | the crates.io landing page's § "Upgrade Discipline" | 3 |
| `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md` | the release's `### Changed` + `### Upgrade notes` | 4 |
| `crates/paigasus-helikon/CHANGELOG.md` | facade note, so the break is not an unremarked cascade patch | 4 |
| `docs/book/src/reference/crates.md` | crate roster version column | 4 |
| `docs/superpowers/specs/2026-08-06-sma-462-…-design.md` | superseded banner | 4 |

---

### Task 1: Refuse pre-envelope inputs in the codec

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activity_input.rs` — module docs (L12-17), `warn_legacy` (L81-92), `ACT_RENDER` doc (L71-78), `ACT_CALL_MODEL` doc (L227-228), `ACT_INVOKE_TOOL` doc (L315-316), the three `from_payloads` legacy arms (L199-221, L286-309, L376-406), and `mod tests` (L468+)
- Test: same file, `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `fn reject_legacy(activity: &str, arity: usize) -> PayloadConversionError` — a private module-level helper. Task 2 does not call it but its tests must not collide with the tests added here.

- [ ] **Step 1: Write the failing rejection tests**

Replace the three existing `*_decodes_legacy_*_payload_shape` tests. Find `render_instructions_decodes_legacy_two_payload_shape` (starts at L468) and replace the whole `#[test] fn …{ … }` block with:

```rust
    /// The pre-envelope two-payload shape must now be **refused**, not decoded.
    ///
    /// Asserts the message's content, not merely that an error occurred: a
    /// variant-only assertion would still pass if the arm returned
    /// `WrongEncoding` (letting the composite silently fall through and losing
    /// the diagnostic in production), or if a copy-paste error passed the wrong
    /// `ACT_*` constant or arity into `reject_legacy`.
    #[test]
    fn render_instructions_rejects_legacy_two_payload_shape() {
        with_ctx(|ctx| {
            let legacy = MultiArgs2(
                "agent-1".to_owned(),
                Some(serde_json::json!({ "tenant": "acme" })),
            );
            let payloads = ctx
                .converter
                .to_payloads(ctx, &legacy)
                .expect("encode legacy");
            assert_eq!(payloads.len(), 2, "legacy shape is two payloads");

            let err = ctx
                .converter
                .from_payloads::<RenderInstructionsInput>(ctx, payloads)
                .expect_err("the pre-envelope shape must no longer decode");
            assert!(
                matches!(err, PayloadConversionError::EncodingError(_)),
                "expected EncodingError, got {err:?}"
            );
            let msg = err.to_string();
            assert!(msg.contains(ACT_RENDER), "must name the activity: {msg}");
            assert!(msg.contains("2 payloads"), "must name the count: {msg}");
            assert!(msg.contains("0.2.2"), "must name the recovery version: {msg}");
        });
    }
```

Replace `call_model_decodes_legacy_two_payload_shape` with:

```rust
    /// The pre-envelope two-payload shape must now be refused — see
    /// `render_instructions_rejects_legacy_two_payload_shape` on why the
    /// message content is asserted rather than just the error variant.
    #[test]
    fn call_model_rejects_legacy_two_payload_shape() {
        with_ctx(|ctx| {
            let legacy = MultiArgs2("agent-1".to_owned(), ModelRequest::new());
            let payloads = ctx
                .converter
                .to_payloads(ctx, &legacy)
                .expect("encode legacy");
            assert_eq!(payloads.len(), 2, "legacy shape is two payloads");

            let err = ctx
                .converter
                .from_payloads::<CallModelInput>(ctx, payloads)
                .expect_err("the pre-envelope shape must no longer decode");
            assert!(
                matches!(err, PayloadConversionError::EncodingError(_)),
                "expected EncodingError, got {err:?}"
            );
            let msg = err.to_string();
            assert!(msg.contains(ACT_CALL_MODEL), "must name the activity: {msg}");
            assert!(msg.contains("2 payloads"), "must name the count: {msg}");
            assert!(msg.contains("0.2.2"), "must name the recovery version: {msg}");
        });
    }
```

Replace `invoke_tool_decodes_legacy_three_payload_shape` with:

```rust
    /// The pre-envelope three-payload shape must now be refused — see
    /// `render_instructions_rejects_legacy_two_payload_shape` on why the
    /// message content is asserted rather than just the error variant.
    #[test]
    fn invoke_tool_rejects_legacy_three_payload_shape() {
        with_ctx(|ctx| {
            let legacy = MultiArgs3(
                "agent-1".to_owned(),
                tool_call(),
                Some(serde_json::json!({ "tenant": "acme" })),
            );
            let payloads = ctx
                .converter
                .to_payloads(ctx, &legacy)
                .expect("encode legacy");
            assert_eq!(payloads.len(), 3, "legacy shape is three payloads");

            let err = ctx
                .converter
                .from_payloads::<InvokeToolInput>(ctx, payloads)
                .expect_err("the pre-envelope shape must no longer decode");
            assert!(
                matches!(err, PayloadConversionError::EncodingError(_)),
                "expected EncodingError, got {err:?}"
            );
            let msg = err.to_string();
            assert!(
                msg.contains(ACT_INVOKE_TOOL),
                "must name the activity: {msg}"
            );
            assert!(msg.contains("3 payloads"), "must name the count: {msg}");
            assert!(msg.contains("0.2.2"), "must name the recovery version: {msg}");
        });
    }
```

Then add the payload-free test (spec §7.3). Put it immediately after `decode_diagnostics_never_leak_payload_bytes`:

```rust
    /// Spec §7.3: the **rejection** diagnostic must be payload-free too.
    ///
    /// `decode_diagnostics_never_leak_payload_bytes` covers only the arity-1
    /// envelope arm. `reject_legacy` is a separate error path whose input
    /// carries real content, so without this test a later edit appending the
    /// offending payload's bytes to the message would ship silently into
    /// Temporal history.
    #[test]
    fn rejection_diagnostics_never_leak_payload_bytes() {
        const SENTINEL: &str = "super-secret-tenant-token";
        with_ctx(|ctx| {
            let legacy = MultiArgs2(SENTINEL.to_owned(), Option::<serde_json::Value>::None);
            let payloads = ctx
                .converter
                .to_payloads(ctx, &legacy)
                .expect("encode legacy");
            let err = ctx
                .converter
                .from_payloads::<RenderInstructionsInput>(ctx, payloads)
                .expect_err("the pre-envelope shape must no longer decode");

            let display = err.to_string();
            let debug = format!("{err:?}");
            assert!(
                !display.contains(SENTINEL),
                "Display leaked payload bytes: {display}"
            );
            assert!(
                !debug.contains(SENTINEL),
                "Debug leaked payload bytes: {debug}"
            );
        });
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p paigasus-helikon-runtime-temporal rejects_legacy`

Expected: **FAIL**. The three tests panic on `expect_err` — the legacy arms still decode successfully, so `from_payloads` returns `Ok`. `rejection_diagnostics_never_leak_payload_bytes` fails the same way.

- [ ] **Step 3: Replace `warn_legacy` with `reject_legacy`**

Replace the whole `warn_legacy` function (L81-92, including its doc comment) with:

```rust
/// Reject a pre-envelope activity input with an actionable diagnostic.
///
/// `EncodingError` rather than `WrongEncoding` is deliberate and load-bearing:
/// the composite converter treats `WrongEncoding` as "not my encoding" and falls
/// through to the next converter, swallowing the message; any other error is
/// returned immediately, so this is what actually reaches the
/// `ActivityTaskFailed` history event.
///
/// The `tracing::error!` is not redundant with that event: the history event is
/// visible only to someone querying Temporal, whereas this reaches the worker's
/// own log pipeline, where alerting lives. Under an unbounded retry policy this
/// logs once per attempt — accepted deliberately, since the volume is itself the
/// signal for a condition that requires operator intervention.
///
/// The message carries the activity name and the payload *count* only, never
/// payload bytes: it lands in Temporal history, a persistence boundary.
fn reject_legacy(activity: &str, arity: usize) -> PayloadConversionError {
    tracing::error!(
        activity,
        legacy_arity = arity,
        "refused a pre-envelope activity input; a worker on 0.2.1 or earlier queued this task"
    );
    PayloadConversionError::EncodingError(
        format!(
            "{activity}: received {arity} payloads — the pre-envelope positional shape \
             (0.2.1 and earlier). This worker decodes only the single-payload envelope. \
             Recovery: re-join a worker on 0.2.2, which decodes both shapes, to this task \
             queue and let in-flight runs drain."
        )
        .into(),
    )
}
```

- [ ] **Step 4: Replace the three legacy decode arms**

In `RenderInstructionsInput::from_payloads`, replace the entire `2 => { … }` arm (L199-221, including its `// Legacy pre-envelope …` comment) with:

```rust
            // Pre-envelope (0.2.1 and earlier): (agent_name, ctx_seed) as two
            // payloads. Recognized only to produce a named error — SMA-484.
            2 => Err(reject_legacy(ACT_RENDER, 2)),
```

In `CallModelInput::from_payloads`, replace the entire `2 => { … }` arm (L286-309) with:

```rust
            // Pre-envelope (0.2.1 and earlier): (agent_name, request) as two
            // payloads — this shape is unchanged since 0.1.x. Recognized only to
            // produce a named error — SMA-484.
            2 => Err(reject_legacy(ACT_CALL_MODEL, 2)),
```

In `InvokeToolInput::from_payloads`, replace the entire `3 => { … }` arm (L376-406) with:

```rust
            // Pre-envelope (0.2.1 and earlier): (agent_name, call, ctx_seed) as
            // three payloads. Recognized only to produce a named error — SMA-484.
            3 => Err(reject_legacy(ACT_INVOKE_TOOL, 3)),
```

Leave the `1 => { … }` envelope arms and `_ => Err(PayloadConversionError::WrongEncoding)` exactly as they are.

- [ ] **Step 5: Update the in-file documentation**

In the module docs, replace the `# Wire shapes` paragraph (L12-17) with:

```rust
//! # Wire shapes
//!
//! Each wrapper encodes to — and decodes from — **one** JSON-object payload.
//! The pre-envelope positional arities (2 payloads for `render_instructions` /
//! `call_model`, 3 for `invoke_tool`) are still recognized, but only to produce
//! a named [`reject_legacy`] error; they are no longer decoded. Upgrading a
//! fleet from 0.2.0 or 0.2.1 therefore requires a stop at 0.2.2, which
//! decodes both shapes — see the crate docs, § "Upgrade Discipline and
//! Determinism".
```

In `ACT_RENDER`'s doc comment (L71), change the first line from `Activity name used in decode diagnostics and legacy-shape warnings.` to:

```rust
/// Activity name used in decode diagnostics and pre-envelope rejections.
```

Make the same substitution in `ACT_CALL_MODEL`'s (L227) and `ACT_INVOKE_TOOL`'s (L315) doc comments, which read `Activity name used in decode diagnostics and legacy-shape warnings. Fully qualified to match the registered `ActivityType` — see `ACT_RENDER`'s doc.` → replace `legacy-shape warnings` with `pre-envelope rejections` in each.

- [ ] **Step 6: Add the doc-comment notes to the arity-rejection tests**

These tests are otherwise unchanged. Add a doc comment above each so a future reader does not "helpfully" add the legacy arity to the `WrongEncoding` set.

Above `render_instructions_rejects_unrecognized_arity`:

```rust
    /// Arity 2 is deliberately absent here: it is `render_instructions`'
    /// former legacy arity and now yields `EncodingError` from `reject_legacy`,
    /// not `WrongEncoding`. Covered by
    /// `render_instructions_rejects_legacy_two_payload_shape`.
```

Above `call_model_rejects_unrecognized_arity`:

```rust
    /// Arity 2 is deliberately absent here: it is `call_model`'s former legacy
    /// arity and now yields `EncodingError` from `reject_legacy`, not
    /// `WrongEncoding`. Covered by `call_model_rejects_legacy_two_payload_shape`.
```

Above `invoke_tool_rejects_unrecognized_arity`:

```rust
    /// Arity 3 is deliberately absent here: it is `invoke_tool`'s former legacy
    /// arity and now yields `EncodingError` from `reject_legacy`, not
    /// `WrongEncoding`. Covered by
    /// `invoke_tool_rejects_legacy_three_payload_shape`. The arity-2 probe below
    /// stays `WrongEncoding` — 2 is `invoke_tool`'s 0.1.x shape, outside the
    /// support window.
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p paigasus-helikon-runtime-temporal`

Expected: **PASS**, all tests. If `render_instructions_content_failure_is_encoding_error`, `call_model_content_failure_is_encoding_error`, or `invoke_tool_content_failure_is_encoding_error` pass, that is expected and *not* proof they are still meaningful — they now exercise the rejection path rather than content decoding. Task 2 fixes that.

- [ ] **Step 8: Format, lint and commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-temporal --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-temporal/src/activity_input.rs
git commit -m "feat(runtime-temporal)!: SMA-484 refuse pre-envelope activity inputs

Replaces the legacy positional decode arms with arms that decode nothing
and return a named EncodingError, and swaps warn_legacy for reject_legacy
(which also logs at error level, so the worker's own log pipeline sees the
condition rather than only Temporal history).

The message names the activity, the payload count and 0.2.2 as the
recovery version, and carries no payload bytes.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
git show --stat HEAD
```

Confirm `git show --stat` lists **exactly one** file.

---

### Task 2: Re-point the content-failure tests at the envelope arm

**Why this is a separate task:** after Task 1 the three `*_content_failure_is_encoding_error` tests **still pass** — they feed a legacy `MultiArgs{N}`, which now hits `reject_legacy` and returns `EncodingError`, exactly what they assert. Nothing fails, and the "recognized arity, bad content" path silently loses all coverage. That is precisely the kind of change worth its own reviewer gate.

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/activity_input.rs` — the three `*_content_failure_is_encoding_error` tests
- Test: same file

**Interfaces:**
- Consumes: `reject_legacy` and the rejection arms from Task 1 (only indirectly — these tests must now avoid the legacy arities).
- Produces: nothing later tasks depend on.

- [ ] **Step 1: Rewrite the three content-failure tests**

Replace `render_instructions_content_failure_is_encoding_error` (including its doc comment) with:

```rust
    /// A recognized arity whose content is wrong must be `EncodingError`, not
    /// `WrongEncoding` — the former short-circuits the composite converter and
    /// surfaces the real diagnostic.
    ///
    /// Since SMA-484 the only recognized arity is 1, so this feeds a single
    /// payload. The bad-content case is a **missing required field**, kept
    /// deliberately distinct from `decode_diagnostics_never_leak_payload_bytes`
    /// (which feeds a bare JSON string at the same arity) so the two tests do
    /// not collapse into duplicates.
    #[test]
    fn render_instructions_content_failure_is_encoding_error() {
        with_ctx(|ctx| {
            // Envelope arity, but `agent_name` is absent and has no serde default.
            let bad = serde_json::json!({});
            let payload = ctx.converter.to_payload(ctx, &bad).expect("encode");
            let err = ctx
                .converter
                .from_payloads::<RenderInstructionsInput>(ctx, vec![payload])
                .expect_err("a missing agent_name must fail");
            assert!(
                matches!(err, PayloadConversionError::EncodingError(_)),
                "expected EncodingError, got {err:?}"
            );
        });
    }
```

Replace `call_model_content_failure_is_encoding_error` with:

```rust
    /// A recognized arity (1, the envelope) whose content is wrong must be
    /// `EncodingError` — see `render_instructions_content_failure_is_encoding_error`.
    ///
    /// Corrupts exactly one field of an otherwise-valid envelope, so the failure
    /// is unambiguously the **wrong type** on `agent_name` rather than a missing
    /// `request`.
    #[test]
    fn call_model_content_failure_is_encoding_error() {
        with_ctx(|ctx| {
            let mut bad = serde_json::to_value(call_model_args()).expect("to_value");
            bad["agent_name"] = serde_json::json!(42);
            let payload = ctx.converter.to_payload(ctx, &bad).expect("encode");
            let err = ctx
                .converter
                .from_payloads::<CallModelInput>(ctx, vec![payload])
                .expect_err("a non-String agent_name must fail");
            assert!(
                matches!(err, PayloadConversionError::EncodingError(_)),
                "expected EncodingError, got {err:?}"
            );
        });
    }
```

Replace `invoke_tool_content_failure_is_encoding_error` with:

```rust
    /// A recognized arity (1, the envelope) whose content is wrong must be
    /// `EncodingError` — see `render_instructions_content_failure_is_encoding_error`.
    ///
    /// Corrupts exactly one field of an otherwise-valid envelope, so the failure
    /// is unambiguously the **wrong type** on `agent_name` rather than a missing
    /// `call`.
    #[test]
    fn invoke_tool_content_failure_is_encoding_error() {
        with_ctx(|ctx| {
            let mut bad = serde_json::to_value(invoke_tool_args()).expect("to_value");
            bad["agent_name"] = serde_json::json!(42);
            let payload = ctx.converter.to_payload(ctx, &bad).expect("encode");
            let err = ctx
                .converter
                .from_payloads::<InvokeToolInput>(ctx, vec![payload])
                .expect_err("a non-String agent_name must fail");
            assert!(
                matches!(err, PayloadConversionError::EncodingError(_)),
                "expected EncodingError, got {err:?}"
            );
        });
    }
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p paigasus-helikon-runtime-temporal content_failure`

Expected: **PASS**, 3 tests.

If a test fails with a *type* error at `serde_json::to_value(call_model_args())`, note that `call_model_args()` returns `CallModelArgs` by value and `CallModelArgs` derives `Serialize` — pass it by value as written. `invoke_tool_args()` likewise returns `InvokeToolArgs`.

- [ ] **Step 3: Confirm the tests are now mutation-resistant**

Temporarily change `render_instructions`' envelope arm from `decode_arg(...)?` to a hand-rolled `.map_err(|_| PayloadConversionError::WrongEncoding)?` and re-run `cargo test -p paigasus-helikon-runtime-temporal content_failure`. It must **FAIL**. Revert the temporary change immediately and re-run to confirm PASS.

This is the check that the test asserts the real property. Do not skip it, and do not commit the temporary change.

- [ ] **Step 4: Verify the whole suite still passes**

Run: `cargo test -p paigasus-helikon-runtime-temporal`

Expected: **PASS**, all tests.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt --all
cargo clippy -p paigasus-helikon-runtime-temporal --all-features --all-targets -- -D warnings
git add crates/paigasus-helikon-runtime-temporal/src/activity_input.rs
git commit -m "test(runtime-temporal): SMA-484 re-point content-failure tests at the envelope arm

These fed a legacy MultiArgs{N} to reach the 'recognized arity, bad
content' path. After the rejection arms landed they still passed, but
against reject_legacy rather than against content decoding, silently
dropping coverage of the only remaining recognized arity.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
git show --stat HEAD
```

---

### Task 3: Rewrite the crate's upgrade-discipline documentation

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/src/lib.rs:339-366` (§ "Upgrade Discipline and Determinism") and `:401-405` (the `mod activity_input` doc)
- Modify: `crates/paigasus-helikon-runtime-temporal/README.md:161`
- Test: `cargo doc` (no unit tests — this task is documentation only)

**Interfaces:**
- Consumes: the behaviour implemented in Task 1.
- Produces: the wording Task 4's CHANGELOG entries stay consistent with.

Spec §5.2 fixes the disposition of each paragraph. Two paragraphs are **kept**, and deleting them would lose a still-correct warning.

- [ ] **Step 1: Replace the SMA-462 wire-change paragraph**

In `lib.rs`, replace the paragraph beginning `//! **SMA-462 wire change (activity inputs are now a single envelope payload).**` and ending `//! rather than being silently misread.` (L339-345) with:

```rust
//! **SMA-484 wire change (activity inputs are envelope-only as of 0.3.0).** Each of
//! `render_instructions` / `call_model` / `invoke_tool` takes one self-describing JSON-object
//! payload, and that is now the **only** shape a worker decodes. The pre-envelope positional
//! shapes (0.2.1 and earlier) are recognized solely to produce a named decode error; SMA-462's
//! 0.2.2 release, which decoded both, is the migration bridge. 0.1.x remains outside the support
//! window and fails closed as before.
//!
//! **Upgrading from 0.2.0 or 0.2.1 requires a stop at 0.2.2**, for activity inputs:
//!
//! | from → to | outcome |
//! |---|---|
//! | `0.2.2` → `0.3.0` | compatible **both** ways — both encode and decode the envelope; no drain needed for this change |
//! | `0.2.1` or earlier → `0.3.0`, directly | **broken both ways** — `0.3.0` cannot read legacy-queued tasks, and a `0.2.1` worker cannot read an envelope |
//! | `0.2.1` or earlier → `0.2.2` → `0.3.0` | works, **provided in-flight runs are drained while the fleet is on 0.2.2** |
//!
//! Throughout this section, *drain* means: stop starting new executions on the task queue, and
//! wait until every execution already on it reaches a **terminal** state — not merely pausing
//! new runs.
//!
//! **If a 0.3.0 worker meets a pre-envelope task anyway**, it logs at `ERROR` and fails the
//! attempt **retryably**, so Temporal re-dispatches. That is the recovery path: any worker on
//! 0.2.2 still polling the queue decodes and executes the task. Re-join one, let in-flight runs
//! drain, then remove it. A run that cannot be drained in an acceptable window is handled with a
//! blue-green task queue (below) or by terminating the execution.
```

- [ ] **Step 2: Rescope the reverse-direction paragraph and correct its retry bounds**

Immediately below, the paragraph beginning `//! The reverse does not hold, and it matters during a rolling deploy:` (L347) and its four numbered bounds (L353-359) stay — they describe a real, still-current hazard — but must be rescoped and corrected. Replace `//! The reverse does not hold, and it matters during a rolling deploy: a **0.2.1-and-earlier**` … through the end of the numbered list with:

```rust
//! **The envelope is unreadable below 0.2.2, and that matters during a rolling deploy.** A
//! **0.2.1-and-earlier** worker handed an envelope payload cannot decode it. It fails the
//! attempt retryably and Temporal re-dispatches until a worker that understands the envelope
//! takes it. The same is true of a 0.3.0 worker handed a pre-envelope payload. Four things bound
//! that recovery:
//!
//! 1. A finite `maximum_attempts` on `model_retry_policy` / `tool_retry_policy` can be exhausted.
//! 2. `WorkflowInput::timeout_ms` interrupts the whole run on its own schedule, regardless of
//!    retry policy.
//! 3. A terminal `render_instructions` failure ends the run; it is not a degraded step.
//! 4. Exhausted `invoke_tool` retries are folded into a tool-error result and fed to the model
//!    rather than failing loudly.
//!
//! **Neither of the first two is on by default.** `render_instructions` is built with no retry
//! policy at all, so the Temporal server default — unlimited attempts — applies; and
//! `WorkflowInput::timeout_ms` is `None` unless set, meaning no deadline. On a default
//! configuration the retry loop is therefore **unbounded**: the run retries indefinitely, writing
//! one `ActivityTaskFailed` event per attempt and consuming workflow history. Do not rely on the
//! failure self-terminating; recovery is operator action.
//!
//! So: **keep the mixed-fleet window short**, and either drain in-flight runs first or ensure
//! retry caps are unlimited and run deadlines generous for the duration of the rollout.
```

- [ ] **Step 3: Rescope the rollback paragraph — do not delete it**

Replace the `//! **Rolling back.**` paragraph (L363-366) with:

```rust
//! **Rolling back.** Once a worker has queued an envelope-shaped activity task, that payload is
//! frozen in the `ActivityTaskScheduled` event and every retry re-delivers it. A rollback to
//! **below 0.2.2** leaves those activities undecodable until the run deadline — which, per the
//! paragraph above, may not exist. **Drain in-flight runs before rolling back below 0.2.2.**
//! Rolling back from 0.3.0 to 0.2.2 is safe: 0.2.2 decodes the envelope.
```

Leave the `**What this buys.**` paragraph (L368-373) and the `**Operational guidance:**` numbered list (L375-388) **unchanged**.

- [ ] **Step 4: Update the `mod activity_input` doc comment**

Replace the doc comment at `lib.rs:401-404` with:

```rust
/// Wire codec for activity inputs: one self-describing envelope payload per
/// activity. The pre-envelope positional shapes (0.2.1 and earlier) are
/// recognized only to produce a named decode error. Private — the envelope
/// types never cross the public API boundary.
```

- [ ] **Step 5: Update the README**

In `crates/paigasus-helikon-runtime-temporal/README.md`, replace the paragraph beginning `Activity inputs are a single self-describing envelope payload as of SMA-462,` (L161) with:

```markdown
Activity inputs are a single self-describing envelope payload, and as of 0.3.0 (SMA-484) that is the **only** shape a worker decodes. **Upgrading from 0.2.0 or 0.2.1 requires a stop at 0.2.2**, which decodes both shapes (0.1.x cannot use that bridge and must drain outright): land the fleet on 0.2.2, drain in-flight runs, then take 0.3.0. 0.2.2 ↔ 0.3.0 is compatible both ways for activity inputs and needs no drain for this change. If a 0.3.0 worker does meet a pre-envelope task it logs an error and fails the attempt retryably — re-join a 0.2.2 worker to the task queue and let the runs drain. Because a queued envelope payload is frozen in history, **drain in-flight runs before rolling back below 0.2.2**. Blue-green task queues remain available. See the [crate docs](https://docs.rs/paigasus-helikon-runtime-temporal) (§ "Upgrade Discipline and Determinism").
```

Leave the preceding paragraph (`Replaying workflows against a different version …`) unchanged.

- [ ] **Step 6: Verify the docs build clean**

Run: `RUSTDOCFLAGS="-D warnings" cargo doc -p paigasus-helikon-runtime-temporal --all-features --no-deps`

Expected: **PASS**, no warnings. A failure here is most likely an intra-doc link or a malformed markdown table in the doc comment.

- [ ] **Step 7: Commit**

```bash
cargo fmt --all
git add crates/paigasus-helikon-runtime-temporal/src/lib.rs crates/paigasus-helikon-runtime-temporal/README.md
git commit -m "docs(runtime-temporal): SMA-484 rewrite upgrade discipline for envelope-only decoding

Documents 0.2.2 as the required stepping stone off the pre-envelope wire,
the mixed-fleet self-heal as the recovery, and the fact that the retry
loop is unbounded on a default configuration - render_instructions
carries no retry policy and timeout_ms defaults to None, so neither of
the two bounds that could stop it is on.

Keeps the rollback warning, rescoped to 'below 0.2.2'.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
git show --stat HEAD
```

---

### Task 4: Changelogs, crate table, and the superseded banner

**Files:**
- Modify: `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md` — the `## [Unreleased]` section
- Modify: `crates/paigasus-helikon/CHANGELOG.md` — the `## [Unreleased]` section
- Modify: `docs/book/src/reference/crates.md:34`
- Modify: `docs/superpowers/specs/2026-08-06-sma-462-temporal-activity-input-compat-design.md` — banner under `**Status:**`
- Test: `mdbook build docs/book`

**Interfaces:**
- Consumes: the wording from Task 3; keep them consistent.
- Produces: nothing.

Hand-written content under `## [Unreleased]` is preserved by release-plz and folded into the generated release section — this is the same pattern SMA-462 used, visible in the `0.2.2` entry which carries both a generated `### Added` line and hand-written `### Changed` / `### Upgrade notes` blocks.

- [ ] **Step 1: Add the crate CHANGELOG entry**

In `crates/paigasus-helikon-runtime-temporal/CHANGELOG.md`, replace the line `## [Unreleased]` with:

```markdown
## [Unreleased]

### Changed

- *(runtime-temporal)* SMA-484 activity inputs are **envelope-only**
  - The pre-envelope positional decode arms are removed. A worker on this version handed one of those payloads logs at `ERROR` and fails the attempt with a named decode error naming the activity, the payload count and the recovery version, instead of executing it.
  - No public Rust API change — the envelope types are crate-internal. The break is on the wire only.

### Upgrade notes

- **Upgrading from 0.2.0 or 0.2.1 requires a stop at 0.2.2.** That release decodes both the envelope and those two releases' pre-envelope positional shapes, so it is the migration bridge: land the fleet on 0.2.2, **drain in-flight runs while it is there**, then take this version. *Drain* means every workflow execution on the task queue has reached a terminal state — not merely that new runs have been paused.
- **0.2.2 ↔ this version is compatible in both directions for activity inputs.** Both encode and decode the envelope, so a rolling 0.2.2 → 0.3.0 upgrade needs no drain on account of this change. (Scope: activity inputs. `WorkflowInput` and activity outputs are unchanged here and carry their own compatibility story.)
- **If a worker on this version meets a pre-envelope task anyway**, the attempt fails *retryably* and Temporal re-dispatches it. Any 0.2.2 worker still polling the queue will decode and execute it — so the recovery is to re-join one, let in-flight runs drain, then remove it. For a run that cannot be drained in an acceptable window, use a blue-green task queue or terminate the execution.
- **Do not expect the failure to self-terminate.** `render_instructions` is built with no retry policy, so the server default of unlimited attempts applies, and `WorkflowInput::timeout_ms` is `None` unless set. On a default configuration the retry loop is unbounded and recovery is operator action.
- **Rolling back below 0.2.2 still requires a drain**, unchanged: a queued envelope payload is frozen in history and re-delivered on every retry. Rolling back to 0.2.2 is safe.
```

- [ ] **Step 2: Add the facade CHANGELOG entry**

The root `Cargo.toml` pins `paigasus-helikon-runtime-temporal` with a caret requirement that excludes `0.3.0`, so release-plz will rewrite the pin and give the facade a **patch** bump whose generated entry reads only "updated the following local packages". Without this note, a facade user on the `runtime-temporal` feature receives a wire-breaking change unremarked.

In `crates/paigasus-helikon/CHANGELOG.md`, replace the line `## [Unreleased]` with:

```markdown
## [Unreleased]

### Upgrade notes

- **`runtime-temporal` feature: the bundled `paigasus-helikon-runtime-temporal` contains a wire-breaking change** (SMA-484 — activity inputs are envelope-only). Despite arriving here as a routine dependency bump, upgrading a Temporal worker fleet from a release built against `paigasus-helikon-runtime-temporal` 0.2.0 or 0.2.1 requires a stop at 0.2.2 and a drain of in-flight runs (0.1.x cannot use that bridge and must drain outright). See that crate's CHANGELOG for the full upgrade notes.
```

- [ ] **Step 3: Correct the crate table's version column**

In `docs/book/src/reference/crates.md`, line 34 currently ends `| published | `0.1.0` |`. That is already stale against the shipped 0.2.2. Change the version cell to:

```
`0.3.0`
```

so the row reads `… | Durable Temporal-backed runner (`TemporalRunner`; crash-resume via Temporal history replay) | published | `0.3.0` |`.

Change **only** the `runtime-temporal` row. Leave every other row alone — correcting the rest is out of scope for this ticket.

- [ ] **Step 4: Add the superseded banner to the SMA-462 spec**

In `docs/superpowers/specs/2026-08-06-sma-462-temporal-activity-input-compat-design.md`, insert immediately after the `**SDK baseline:** …` line (the last line of the header block, before `## 1. Context & problem`):

```markdown
> **Superseded in part by [SMA-484](https://linear.app/smaschek/issue/SMA-484).** The legacy
> pre-envelope decode arms this design added were removed in 0.3.0; §4.7's warning-goes-silent
> exit criterion was never met and was retired as unobservable to a telemetry-free crates.io
> library. Every claim below about decoding the 0.2.x positional shapes — §2, §4.4, §4.7, §6.1,
> §7 and §10 — describes 0.2.2 only. The envelope design itself stands.
```

- [ ] **Step 5: Verify the book builds**

Run: `mdbook build docs/book`

Expected: **PASS** with no output beyond the build log. `[output.linkcheck] warning-policy = "error"` means a broken link fails the build. If `mdbook` is not installed, say so and skip this step rather than installing it — the `book-build` CI job will catch it.

- [ ] **Step 6: Commit**

```bash
git add crates/paigasus-helikon-runtime-temporal/CHANGELOG.md crates/paigasus-helikon/CHANGELOG.md docs/book/src/reference/crates.md docs/superpowers/specs/2026-08-06-sma-462-temporal-activity-input-compat-design.md
git commit -m "docs(runtime-temporal): SMA-484 add changelog upgrade notes and mark SMA-462 superseded

Includes a facade changelog note: the root Cargo.toml pin means the wire
break otherwise reaches facade users as an unremarked cascade patch bump.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
git show --stat HEAD
```

Confirm `git show --stat` lists **exactly four** files.

---

### Task 5: Full-gate verification

**Files:** none modified (unless a gate fails).

**Interfaces:**
- Consumes: everything from Tasks 1-4.
- Produces: a branch ready for a PR.

- [ ] **Step 1: Run every CI gate that can run locally**

Run each, in order, and wait for each to finish before starting the next:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-features --all-targets -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

Expected: all **PASS**.

Two known non-signals, so a red result is not misread:
- On macOS, ~48 `bedrock` failures mentioning `NATIVE_ROOTS` track the *checkout path*, not the code. This branch is already in a worktree, which is the configuration that passes — if they appear anyway, they are unrelated to this change.
- `cargo doc` emits a non-fatal filename-collision warning between the `paigasus-helikon` facade lib and the CLI binary of the same name. Expected and by design; do not "fix" it.

- [ ] **Step 2: Verify the diff matches the plan**

```bash
git diff --stat main...HEAD
git log --oneline main..HEAD
```

Expected: **6 files** changed across **4 implementation commits** plus the 2 spec commits already on the branch. The changed files are exactly:

```
crates/paigasus-helikon-runtime-temporal/src/activity_input.rs
crates/paigasus-helikon-runtime-temporal/src/lib.rs
crates/paigasus-helikon-runtime-temporal/README.md
crates/paigasus-helikon-runtime-temporal/CHANGELOG.md
crates/paigasus-helikon/CHANGELOG.md
docs/book/src/reference/crates.md
```

plus `docs/superpowers/specs/*` and `docs/superpowers/plans/*`.

If any `Cargo.toml` appears in the diff, that is a mistake — revert it. release-plz owns the version bump.

- [ ] **Step 3: Confirm no stray legacy references remain**

```bash
grep -rn 'warn_legacy\|decodes_legacy\|0\.2\.0–0\.2\.1' \
  crates/paigasus-helikon-runtime-temporal/ --include='*.rs' --include='*.md'
```

Expected: matches only in `CHANGELOG.md`'s historical `0.2.2` entry, which is an immutable record and must not be edited. A match in `src/` or in `README.md` means something was missed.

`legacy_arity` is deliberately **not** in that pattern: `reject_legacy` still emits it as a `tracing` field, so searching for it would flag intended code. Likewise "0.2.0 or 0.2.1" is now legitimate prose in the migration guidance (see the version-string constraint above), so it is not a stray-reference marker either.

Then assert the two version scopes have not bled into each other. **"0.2.1 or earlier" must not appear in migration guidance** — it over-scopes the bridge to include 0.1.x, which 0.2.2 cannot rescue:

```bash
grep -rn '0\.2\.1 or earlier' crates/ --include='*.rs' --include='*.md'
```

Expected: **exactly one** match — `reject_legacy`'s `tracing::error!` message in `src/activity_input.rs`, where the phrase is correct because it names the shape that *arrived*. Any other match — in `lib.rs`'s upgrade section, the README, or either CHANGELOG's `[Unreleased]` block — is the bug this check exists to catch. (Matches inside a CHANGELOG's already-released sections are immutable history; leave them.)

- [ ] **Step 4: Verify the commit types will satisfy `convco`**

```bash
convco check main..HEAD
```

Expected: **PASS**. Note the PR *title* — not any individual commit — is what release-plz reads on a squash merge, and it must be:

```
feat(runtime-temporal)!: SMA-484 remove the pre-envelope activity-input decode arms
```

The `!` drives the `0.2.2` → `0.3.0` minor bump. The subject must start lowercase after `SMA-484 ` (`pr-title.yml`'s `subjectPattern` rejects a leading capital), and the full `type(scope):` prefix is required independently of that rule.

---

## Self-Review

**Spec coverage:**

| spec section | task |
|---|---|
| §4.1 `reject_legacy` incl. `tracing::error!` and the three message properties | 1 (steps 3, 1) |
| §4.2 the three decode arms; `decode_arg` left alone | 1 (step 4) |
| §4.3 module docs, `warn_legacy` doc, three `ACT_*` docs | 1 (steps 3, 5) |
| §5 docs table — lib.rs, README | 3 |
| §5 docs table — CHANGELOGs, book, SMA-462 banner (D7) | 4 |
| §5.1 upgrade-note content, items 1-7 | 3 (steps 1-3), 4 (step 1) |
| §5.2 per-paragraph lib.rs dispositions | 3 (steps 1-4) |
| §6.1 unbounded-by-default correction | 3 (step 2), 4 (step 1) |
| §6.2 non-retryable infeasible | documented in the spec; no code change, correctly no task |
| §7.1 converted rejection tests | 1 (step 1) |
| §7.2 re-pointed content-failure tests | 2 |
| §7.3 payload-free rejection test | 1 (step 1) |
| §7.4 arity-rejection doc notes | 1 (step 6) |
| §7.5 unchanged tests | untouched by construction |
| §7.7 no live-suite re-run | no task, per the spec's explicit call |
| §8 release mechanics, `!` in the PR title | 5 (step 4) |
| §8.1 facade CHANGELOG + post-merge pin check | 4 (step 2); the post-merge check is Stage 5/6 work, noted below |
| §9 risks | documentation only, carried by tasks 3 and 4 |

**Post-merge item not covered by any task** (it cannot be — it happens after the PR merges): verify the release-plz PR rewrote `[workspace.dependencies]`'s `paigasus-helikon-runtime-temporal` pin to `0.3.0`. A stale pin fails the whole workspace build. Carry this into the PR description.

**Placeholder scan:** no `TBD`/`TODO`/"handle edge cases"/"similar to Task N". Every code step carries the literal code to write.

**Type consistency:** `reject_legacy(activity: &str, arity: usize) -> PayloadConversionError` is defined once in Task 1 step 3 and called with exactly that signature in step 4 (`ACT_RENDER`/2, `ACT_CALL_MODEL`/2, `ACT_INVOKE_TOOL`/3). Test helper names used in later steps — `with_ctx`, `render_args`, `call_model_args`, `invoke_tool_args`, `tool_call` — all already exist in the test module and are unmodified. `MultiArgs2`/`MultiArgs3` remain imported and used.
