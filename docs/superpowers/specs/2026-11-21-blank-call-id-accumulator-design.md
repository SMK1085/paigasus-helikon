# SMA-621: refuse parallel blank-`call_id` tool calls in `ModelTurnAccumulator`

Status: draft for approval (revised after the adversarial spec challenge).
Size: small change. The behaviour change is in `core`. The other edits are docs and
tests in `providers-litellm`, `runtime-temporal` and the conformance suite.

## Intent

`ModelTurnAccumulator::observe` groups `ToolCallDelta`s by `call_id`
(`core/src/model.rs:594-600`). It does not exempt the blank id `""`. So N parallel
calls that arrive under `""` become one `Item::ToolCall`. The first name wins and the
arguments of all calls concatenate. On `main` this has three outcomes:

1. **Loud, wrong cause.** Two calls carry arguments. The concatenation
   (`{"p":1}{"q":2}`) fails to parse. `finish()` returns a `serde_json` "trailing
   characters" error, and the run fails with no statement of the real cause.
2. **Silent loss.** Two calls carry no arguments. `finish()` returns `Ok` with one
   call. The second call is lost and nothing reports it.
3. **Silent misattribution.** The first call carries no arguments and the second
   call does: `("", Some("delete_all"), "")`, then `("", Some("search"), "{\"q\":1}")`.
   The joined text parses. `finish()` returns `Ok`, and **`delete_all` runs with
   `search`'s arguments**. Both chat translators can emit this shape: a delta with no
   name fragment flushes the name with empty arguments (`openai/chat.rs:842-849`,
   `providers-litellm/src/stream.rs:580-587`).

The goal: the accumulator must never guess, and a failure must name the real cause.

No blank-id capture exists in the repo (SMA-616 spec), and no known backend is
recorded as one that sends `"id": ""`. So the effect of outcomes 2 and 3 changing
from `Ok` to `Err` on real users is not known. `ModelSettings` has no
`parallel_tool_calls` setting (`model.rs:137-157`), so a user who hits the new error
has no workaround in this crate.

## Decision (acceptance criterion 1)

The ticket offers options 1, 2 and 3. Sven picked "Approach A" on 2026-11-21. It is a
`core`-only form of ticket option 2.

| Row | Ticket option | What it does | Result and reason |
|-----|---------------|--------------|-------------------|
| A | 2, limited to `core` | If two or more name-carrying deltas arrive under `""`, `finish()` returns an error that names the ambiguity. | **Picked.** It is the only approach that never guesses. It changes no public signature and no translator. Its signal ("a second name is a new call") is the rule that `agui/map.rs:45-46, 62` already uses, so `core` and the AG-UI mapper agree on what a call is. |
| Order buckets | 1 | A new name under `""` starts a new bucket. Nameless deltas go to the latest bucket. | **Rejected.** The translators emit blank-id deltas interleaved: `withheld_upgrades_keep_parallel_blank_calls_merged` emits name, name, args, args. The `index` that told the calls apart does not exist at the event layer. Order buckets give every argument to the last call. If that call has no arguments of its own, the JSON parses and the wrong tool runs with another call's arguments. |
| Split self-contained deltas | 1, a narrow form | If two or more names arrive under `""` and no nameless delta carries arguments, make one item per name. | **Rejected.** It does not guess, but it makes two or more items with the same `call_id: ""`. Downstream code matches on `call_id`: `loop_state.rs:445` (`deferred.iter().any(\|p\| p.call_id() == c.call_id)`) marks both calls pending when only one is deferred, `resume_exec.rs:176` resolves to the first match, and the provider request translators pair each tool result to its call by id. Two items with one id break all three. |
| Synthetic ids | not in the ticket | `providers-litellm` and `openai/chat` replace `""` with an id made from `index`. | **Rejected.** It reverses the SMA-619 decision to keep such a call under `""`. (Gemini already makes `fc_{n}` ids when the backend sends none, so a synthetic id is not new to the repo. The SMA-619 reason alone is enough.) |
| Do nothing | 3 | Keep the docs and the pinning test. | **Rejected.** It does not satisfy acceptance criterion 2, and it keeps outcome 3. |

Ticket option 2 refuses **every** blank-id call and needs every translator to change.
Approach A refuses only the ambiguous case. A single blank-id call keeps its current
behaviour.

## Design

### Contract

The `ModelEvent::ToolCallDelta` doc comment (`model.rs:183-197`) now states the
invariant that the detection needs: **each blank-id call carries exactly one
name-carrying delta**. It says today only that two such calls "each carry a name".
All six translators already comply. The challenge checked each one: litellm once per
key, openai/chat once per index, openai/responses once per item, anthropic and
bedrock once per block, gemini once per part. This is a doc change to the contract
that records the current behaviour. It does not change what any translator emits.

### Detection

- `ModelTurnAccumulator` counts the deltas with `name: Some(_)` that it observes
  under `call_id == ""`, and keeps those names in arrival order.
- `Some("")` counts. It is a name-carrying delta. No translator emits it today.
- Equal names count separately. `"alpha"`, `"alpha"` are two calls under the
  contract, so the count does not de-duplicate.
- A count of 0 or 1 keeps the current behaviour exactly: the deltas merge into one
  bucket and the item has `call_id: ""`.
- Non-blank ids keep the current behaviour exactly, including first-name-wins.

### Failure

- `finish()` checks for the ambiguous case **before** it parses any arguments, for
  every bucket. So the ambiguity error replaces any `serde_json` error in the same
  turn, and outcomes 2 and 3 fail too.
- The whole turn fails, also when a valid non-blank call is in the same turn and
  assistant text is present. That is the same outcome as today's outcome 1. Keeping
  the text and dropping the calls would hand the loop a turn that silently lost its
  tool calls, which is the class of defect this change removes.
- The return type stays `Result<ModelTurn, String>`. No public signature changes.
- The message starts with the stable prefix `ambiguous tool calls:`. Then it gives the
  count and the names. Proposed text:
  `ambiguous tool calls: 2 parallel tool calls arrived under a blank call_id (names: "alpha", "beta"); their arguments cannot be attributed to a call`.
- Message constraints. Tool names come from the model, and the message reaches
  clients through `AgentEvent::RunFailed` (`agent.rs:1514`) and AG-UI.
  - Format each name with `{:?}`, so control characters are escaped.
  - List at most 8 names, then write `and N more`. Cut each name at 64 characters.
  - Never put argument text in the message. Arguments can contain secrets.
- `finish()` also writes a `tracing::warn!` with the count and the names (same
  constraints). The reason: `agent.rs:1511-1512` records only
  `otel.status_code = ERROR` on the span, not the message. `core` already depends
  on `tracing`.
- Callers need no change. `agent.rs:1508-1516` turns the `Err` into
  `AgentError::Other` and `RunFailed { error }`. `runtime-temporal`
  (`activities.rs:97-111`) turns it into `ErrorKindPayload::Other`, which is a
  non-retryable failure (`activities.rs:265-269`). `RetryingModel` retries only
  before the first event (`runtime-tokio/src/retry.rs:133-137`), so a retry cannot
  replay deltas into a false positive.

### Limits (accepted residual shapes)

Approach A is correct only for streams that obey the contract above. Two shapes from a
malformed backend remain, and this change accepts both:

- **False positive.** A backend moves one blank-id call across two wire keys (in
  chat, two `index` values; `canonicalize` does not alias them because it returns
  early for `""` at `chat.rs:552-561`). Two names go out for one call. Today that
  call can return `Ok`. After the change it returns the ambiguity error. A loud
  failure on a malformed stream is acceptable.
- **False negative.** A nameless blank-id call (for example the litellm cross-key
  split described at `chat.rs:686-690`) merges with one named call. The count is 1,
  so detection does not fire, and the named tool can run with the other call's
  arguments. The cause is a translator defect, not an accumulator defect. It stays
  out of scope.

## Updates

1. **New `core` tests** in `model.rs` `model_turn_tests`. Each test that changes
   behaviour must FAIL on `main` first (acceptance criterion 3), and its doc comment
   says "Confirmed to FAIL against `main`", as the repo already does
   (`litellm stream.rs:2162-2165`).
   - Two blank-id calls with distinct arguments: `Err`, and the message starts with
     `ambiguous tool calls:` and contains both names. (`main`: a `serde_json` error.)
   - Two blank-id calls with no arguments: `Err`. (`main`: `Ok`.)
   - Outcome 3, misattribution: `Err`. (`main`: `Ok` with the wrong arguments.)
   - Equal names (`alpha`, `alpha`): `Err`. This kills a mutant that de-duplicates.
   - An ambiguous `""` bucket plus a valid `c1` call: `Err` for the whole turn.
   - An ambiguous `""` bucket plus invalid JSON under `c1`: the ambiguity error, not
     the JSON error. This pins "check before parse".
   - The message does not contain argument text.
   - Controls that pass on `main` and after the change: one blank-id call gives one
     `Ok` item with `call_id: ""`; one blank-id call whose nameless delta comes before
     its named delta gives `Ok`; two non-blank calls give two items.
2. **Docs in `model.rs`**: the `ToolCallDelta` comment at `194-197` (replace "does not
   follow that advice: it deliberately merges blank-id calls" with the new rule and
   add the invariant from "Contract"), the `ModelTurn` doc, and the `finish()` doc at
   `622-623`, which says it fails only on invalid JSON.
3. **`runtime-temporal/src/activities.rs:80-81`**: the doc says `finish()` fails only
   on invalid JSON. Add the ambiguity case.
4. **`withheld_upgrades_keep_parallel_blank_calls_merged`**
   (`providers-litellm/src/stream.rs:2391`): rename it (proposed:
   `withheld_upgrades_make_parallel_blank_calls_fail_loudly`). Assert that the error
   starts with `ambiguous tool calls:`, not only `is_err()`. Rewrite the stale text at
   `stream.rs:2352-2369, 2386-2389, 2424-2429`. The text at `~2228` describes the
   single-call shape and stays.
5. `openai/chat.rs:692` and `openai/responses.rs:409, 1166` describe translator
   behaviour or a single-call parse failure. They need no edit. The builder confirms
   this while doing step 4.
6. **Conformance assertion 7** (`tests/provider-stream-conformance/src/check.rs`):
   **limit "exactly one" to non-blank ids**, and for `""` require at least one
   name-carrying delta. Record this reason in the comment: assertion 7 grades
   translators. Two names under `""` are what the contract requires a translator to
   emit for two blank-id calls, so `ToolNameNotExactlyOnce { call_id: "", count: 2 }`
   would blame a correct translator. A blank-id group with no name is still a
   translator defect, so the "at least one" rule keeps that part of the check. The
   refusal of the ambiguous turn is a consumer decision, and the `core` tests pin it.
   Add a conformance unit test for both rules if `check.rs` has a test module.

## Release

- No hand-bumped version. The change is a `fix` with no signature change.
- release-plz bumps by file path. `core`, `providers-litellm` (test and doc edit) and
  `runtime-temporal` (doc edit) each get a patch bump, and the facade gets a cascade
  bump. This is expected and harmless. No crate gets a breaking bump, so
  `check-breaking-cascade.sh` does not apply.
- The conformance suite is a test crate. Check whether it is published before you
  state its bump in the PR.

## Book and README

No edit needed. `docs/book/src` and the `core` README do not mention the blank-id
merge. `observability-evaluation.md:347-349` cites `invalid tool args for call_id=…`,
and that text stays correct for non-blank ids.

## Non-goals

- No change to any translator's event output.
- No change for a single blank-id call. It still dispatches with `call_id: ""`.
  Whether a backend accepts `""` on replay is a separate question.
- No change for a nameless blank-id call. With 0 names under `""` the turn still
  yields `Item::ToolCall { call_id: "", name: "" }` (`model.rs:545-549`), and the loop
  dispatches it with no check (`loop_state.rs:335-345`).
- No change for non-blank ids, including the SMA-617 name-dedup defect in
  `openai/responses`.
- No typed error for `finish()`. A typed error is a breaking `core` change. The
  stable message prefix is the interim signal.

## Anti-goals

- For streams that obey the contract, the change must never produce a turn in which a
  tool runs with another call's arguments.
- The change must never turn a loud failure into a silent loss.

## Left to the builder

- The exact wording of the message after the prefix, inside the constraints above.
- Whether the count and names live on `ModelTurnAccumulator` or on the `""`
  `ToolCallAccum`.

## Challenge changelog (2026-11-21)

Folded in:

- MAJOR: added outcome 3 (silent misattribution on `main`) to Intent and a test for it.
- MAJOR: wrote the "exactly one name per blank-id call" invariant into the contract,
  limited anti-goal 1 to conforming streams, and recorded both residual shapes.
- MAJOR: added the "split self-contained deltas" row with the duplicate-`call_id`
  reasons.
- MAJOR: fixed the assertion 7 contradiction. Limited "exactly one" to non-blank ids
  and kept an "at least one" rule for `""`.
- MINOR: corrected the synthetic-id row, mapped each row to a ticket option, added the
  message constraints, specified `Some("")`, added the missing tests, corrected the
  stale-reference list, added the nameless blank-id non-goal, completed the release
  note, and recorded the book and README result.
- QUESTION: recorded that no blank-id backend is known, added the stable prefix,
  added the `tracing::warn!`.

Not folded in:

- A follow-up ticket for a typed `finish()` error. It is a breaking change with no
  current consumer that needs it. I offer it to Sven at the gate and do not file it.
