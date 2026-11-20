# SMA-816: `ToolDeferred` conformance test in the sessions testkit

Status: approved at Gate 1 by Sven on 2026-11-20, after the spec challenge.

## Intent

The sessions testkit is on crates.io (SMA-805, `0.1.0`; `0.1.1` after the core 0.6 cascade). It does not yet test `SessionEvent::ToolDeferred`, which core `0.6.0` added (SMA-803).

A host stores a deferral in its session and later resumes the run from it. The resume reads the `ToolDeferred` fields, the `ToolCall` items of `snapshot()` (`ResumePlan::from_events` fails with `CallNotInHistory` if their `call_id` or `name` is wrong), and the `ToolReturned` events that close a deferral. If a backend loses or changes any of these, the resume runs the wrong tool call, runs it with the wrong arguments, or fails. The always-on agent host runs the testkit against its own `PgRunSession` (ADR-0028, decision (b)). It needs the suite to catch such a backend.

## Goals

- A new public sub-test, `run_tool_deferred`, and `run_all` calls it last.
- The sub-test proves that a backend:
  1. returns every field of every event of a log with deferrals unchanged and in order;
  2. gives a `snapshot()` that equals the projection of the appended log, so `ToolDeferred` adds no item and every `ToolCall` and `ToolResult` is exact;
  3. gives a log on which `open_deferrals` finds exactly the open deferrals, with the correct fields;
  4. reads correctly after a watermark inside an `append` batch.
- The memory, SQLite, Postgres and Redis backends pass. They get the test through their existing `run_all` call, so their test files do not change.
- The compacting wrapper passes `run_tool_deferred` (D4).
- Negative tests prove that the sub-test fails for each broken backend in D6.
- The testkit README, the crate rustdoc and `docs/book/src/concepts/sessions.md` name the new sub-test.

## Non-goals

- No change to `paigasus-helikon-core` or to any backend.
- No test of `ResumePlan`, the runner or the resume flow. Core tests those. The testkit tests storage only. The D1 log is a storage fixture and is not a resumable run (see D1), so nobody must add a `ResumePlan` assertion to it.
- No repeated `call_id` in the fixture. Core permits a call id to repeat in one run, but this is a separate storage property. It is a possible follow-up.
- No hand version bump. See D5.

## Anti-goals

- The test must not pass for a backend that keeps the event kind but changes a field. Examples: it drops or reorders `handoff_path`, changes `args`, returns the `ToolCalled` args for the deferral, or changes a `ToolCall` name in the snapshot.
- The test must not depend on JSON key order, or on a timestamp precision finer than one second.

## Decisions

### D1. The test data

The domain is personal finance, as in the other examples of the repository. Each event has a different whole-second `ts`: event `i` has `Timestamp::from_second(1_700_000_000 + i)`. So a backend that mixes `ts` values between events fails. The log is, in this order:

| # | Event | Notes |
|---|-------|-------|
| 0 | `UserMessage` "pay my rent" | Normal event before the tool calls. |
| 1 | `ToolCalled` `c1` `transfer_funds` | `args` M (the model's arguments, below). |
| 2 | `ToolDeferred` `d1` for `c1` | `args` E (the effective arguments, below), two-line `prompt`, `handoff_path = ["triage", "payments"]`. |
| 3 | `ToolCalled` `c2` `close_account` | `args` `{"account": "acct-9"}`. |
| 4 | `ToolDeferred` `d2` for `c2` | Same `args`. `handoff_path = []`. |
| 5 | `ToolCalled` `c3` `lookup_balance` | `args` `{}`. |
| 6 | `ToolDeferred` `d3` for `c3` | Same `args`. `handoff_path = ["triage"]`. |
| 7 | `ToolReturned` `c3` | Text "balance: 40.00". This closes `d3`. |

- **`args` E** is a nested object that exercises JSON round trips:
  - a string with non-ASCII text and a 4-byte character ("Miete für März 💶");
  - a string with a quote, a backslash and a tab;
  - a float (`1250.75`, exact in binary);
  - a large integer (`9_007_199_254_740_993_u64`, which is 2^53 + 1);
  - a negative integer (`-42`);
  - `true`, `null`, an array, and a nested object.
- **`args` M** is E with a different amount and without one key. In core, `ToolDeferred` holds the effective arguments after a `PreToolUse` replacement, so they can differ from the `ToolCalled` arguments. With different values, a backend that stores the arguments once per `call_id` fails.
- No value has a NUL character, because Postgres `JSONB` rejects it. No float has more than 15 significant digits, because serde_json needs its `float_roundtrip` feature for those.
- The `d1` prompt has two lines and a trailing space, so a backend that trims text fails.
- The deferral ids are fixed UUID v4 strings in lowercase and with hyphens. This is the form that core makes (`Uuid::new_v4().to_string()`), and the form that a Postgres `uuid` column returns.
- `d2` has an empty `handoff_path`. This is an edge case that only a custom agent makes, because `LlmAgent` puts its own name first. The path tests `agent() == None` and an empty array round trip.
- `ResumePlan::from_events` rejects this log, because events 3 to 6 follow the first open deferral and the open deferrals have different paths. That is acceptable, because the testkit tests storage, not resume.

`append` gets the events in two calls: events 0 to 4, then events 5 to 7. This also checks that a deferral survives across appends.

### D2. The assertions

Each `append`, `events` and `snapshot` call uses `.expect(...)` with a message that names the call, for example "append batch 1 (events 0-4)". So a backend that rejects the `tool_deferred` kind gives a clear failure.

1. **Round trip.**
   1. `events(None)` returns 8 events, and the kinds are in the order of D1 (`SessionEvent::kind()`).
   2. For each `ToolDeferred`, each field equals the appended value. The fields are `deferral_id`, `call_id`, `name`, `args`, `prompt`, `handoff_path` (order matters) and `ts`. The failure message names the deferral and the field, for example "ToolDeferred d1: handoff_path".
   3. As a backstop, `serde_json::to_value(&got)` equals `serde_json::to_value(&appended)`. This covers every field of every event, `ts` included, and it ignores JSON key order.
2. **Projection.**
   1. `snapshot().messages` has 5 items.
   2. `serde_json::to_value(&snapshot().messages)` equals `serde_json::to_value(&project(&appended).messages)`. Here `appended` is the testkit's own input, not the backend output. So a backend that keeps its own `snapshot()` (as a custom session can) must still match the projection exactly.
3. **Open deferrals.**
   1. `open_deferrals(&got)` equals `[d1, d2]` as `PendingToolCall` values (they have `PartialEq`), in log order.
   2. `d1.agent()` is `Some("payments")`, and `d2.agent()` is `None`.
   3. `d3` is absent, because event 7 closes it.
4. **Watermark.**
   1. `events(Some(SequenceId(4)))` returns events 5 to 7.
   2. `events(Some(SequenceId(5)))` returns exactly events 6 and 7. This position is inside the second `append` batch, so a backend that gives every event of a batch the same sequence fails.
   3. Both checks compare the kinds and the `d3` fields.

### D3. Signature and dependencies

- `pub async fn run_tool_deferred<F, Fut>(make: F) where F: Fn() -> Fut, Fut: Future<Output = Arc<dyn Session>>`. These are the same bounds as the other sub-tests. The function has a `///` doc, because `missing_docs` and `-D warnings` stop the docs gate without one.
- The testkit adds `serde_json = { workspace = true }` as a normal dependency. The workspace does not enable `arbitrary_precision` or `preserve_order`.
- The testkit matches on `SessionEvent::ToolDeferred { .. }` with a wildcard arm, because `SessionEvent` is `#[non_exhaustive]`. It builds expected deferrals with `PendingToolCall::new(...)`, because `PendingToolCall` is `#[non_exhaustive]`. It builds events as variant literals, so that each `ts` is fixed. The `ToolDeferred` variant is not `#[non_exhaustive]`.
- The core pin stays at the workspace value (`0.6.0`).

### D4. The compacting wrapper

`CompactingSession` does not compact while `open_deferrals` of the log is not empty (`compacting_session.rs`, the open-deferral check in `maybe_compact`). In the D1 log, `d1` and `d2` stay open after both appends.

So a new test file, `crates/paigasus-helikon-sessions-testkit/tests/compacting.rs`, runs `run_tool_deferred` (not `run_all`) against `CompactingSession<MemorySession>`. The wrapper has `threshold(1)` and a fake `Model` that panics in `invoke`. The test proves that the wrapper stores and returns deferrals exactly, and that it does not compact over an open deferral.

`run_all` is not used here. With `threshold(1)`, the other sub-tests would start a compaction, and the fake model would panic. The dev-dependencies for the fake model (for example `async-trait` and `futures-util`, from the workspace) are dev-only, so the published manifest does not change.

### D5. Release

- Commit type `feat(sessions)`. release-plz bumps the testkit to the next patch in the next `chore: release` PR. This PR does not bump a version by hand, so it does not publish on its own merge.
- Core `0.6.0` is on crates.io and has every API that the test uses. The verification proves this with `cargo publish --dry-run`.
- The facade does not depend on the testkit, so there is no facade cascade.
- release-plz writes the CHANGELOG entry. No hand edit.

### D6. Negative tests

A new test file, `crates/paigasus-helikon-sessions-testkit/tests/broken_backends.rs`, wraps `MemorySession` in small sessions that corrupt one thing on read. Each test is `#[should_panic(expected = "...")]` with the message that names the field. The corruptions are:

1. reverse `handoff_path` of a `ToolDeferred`;
2. send each `args` number through `f64` (the large integer changes);
3. copy the `ToolCalled` args into the `ToolDeferred` with the same `call_id`;
4. change the `name` of a `ToolCall` item in `snapshot()`;
5. swap two adjacent events;
6. give each event of an `append` batch the same sequence (a watermark inside the batch returns nothing).

These tests prove the anti-goals. They are in the testkit's own `tests/`, so they are not part of the published API.

## Docs

The stability text must not name a version that does not exist yet. It gives `=0.1.1` as the example exact pin, and it says that a new sub-test arrives in a patch release. It adds one sentence: a patch release can move the core requirement (0.1.0 uses core 0.5, and 0.1.1 uses core 0.6), so an exact pin must match your core version.

The sites to edit:

- **Crate rustdoc** (`src/lib.rs`): "four" becomes "five" in two places, a line for `run_tool_deferred` is added, and the stability text changes as above.
- **Testkit README:**
  - the intro paragraph lists the five tests;
  - the "Run a subset" section names the new sub-test;
  - the stability text changes as above.
- **`docs/book/src/concepts/sessions.md`:** a new row in the conformance table, and one sentence that says that the compacting wrapper runs `run_tool_deferred`.

## Verification

1. Run the four backends' conformance tests, plus the testkit's own tests:
   - Use local Docker images of `postgres:17` and `redis:7`, the same images as `ci.yml`.
   - Set `HELIKON_TEST_POSTGRES_URL` and `HELIKON_TEST_REDIS_URL`.
   - Confirm that the output has no `SKIP` line.
2. Run `cargo package -p paigasus-helikon-sessions-testkit` and `cargo publish -p paigasus-helikon-sessions-testkit --dry-run`. Both build against registry core 0.6.0.
3. Run the full local CI gate list from `CLAUDE.md`.
4. On the PR, confirm that the `sessions-it` job ran the suites. It must not print "No session-related changes".
5. Mutation check: temporarily weaken each assertion group of D2. For each one, confirm that a D6 negative test fails. Then restore the assertion.

## Left to the builder

- The exact wording of the assertion messages. Each message must name the event and the field, and each D6 `expected` string must match it.
- Private helper functions to build the events.
- The exact amount and the dropped key in `args` M.

## Approaches considered

1. **One sub-test with a mixed log (chosen).** One function, one fresh session, the log of D1. It tests deferrals together with other events and with `open_deferrals`, and it follows the shape of the other sub-tests.
2. **Several small sub-tests** (`run_tool_deferred_round_trip`, `run_tool_deferred_projection`, `run_open_deferrals`). This gives clearer names when one fails. But it adds three public functions to a stable API, and each needs a fresh session.
3. **Only a JSON comparison of whole events.** The SMA-805 spec planned this, because `SessionEvent` has no `PartialEq`. It covers every field, but its failure message does not name the field. So this spec uses both: per-field checks for clear messages, and the JSON comparison as a backstop.
