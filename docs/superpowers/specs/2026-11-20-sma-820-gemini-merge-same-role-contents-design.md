# SMA-820: Gemini translator merges same-role contents and names each tool result from its own call

Linear: [SMA-820](https://linear.app/smaschek/issue/SMA-820/gemini-translator-sends-two-adjacent-user-contents-after-a-tool-result).
Size: a small change in the Gemini translator. Files: `crates/paigasus-helikon-providers-gemini/src/translate/request.rs`,
`.../src/translate/mod.rs` (inline asserts in `snap_parallel_same_name_tool_calls`), the snapshots in
`.../src/translate/snapshots/`, and `.../tests/live.rs`.

## Intent

`items_to_contents` makes one Gemini content for each core `Item`. Two defects follow.

1. **Adjacent same-role contents.** A tool result becomes a `role: user` content with a
   `functionResponse` part. When a user message comes directly after it, the request has two
   adjacent `role: user` contents. Known sources of this shape:
   - a turn-boundary checkpoint that returns `Inject(..)` after a tool turn (PR #346);
   - a resumed run after a deferred tool call;
   - a synthetic cancel result followed by the input of the next run;
   - the handoff transcript note after a user message, when the transfer turn had no text
     (`crates/paigasus-helikon-core/src/loop_state.rs:770-789`);
   - the `CompactingSession` prompt after a snapshot that ends in a tool result.
   Parallel calls also give adjacent `model` contents (one per `ToolCall`) and adjacent `user`
   contents (one per result).
2. **Wrong name on a repeated call id.** The translator resolves a `functionResponse` name from one
   global `call_id -> name` map, and the last call wins. `stream.rs` makes the fallback ids
   `fc_{n}` per stream, so two turns can both have a call `fc_0`. Then an earlier result gets the
   name of a later call (turn 1 `search`/`fc_0`, turn 2 `pay`/`fc_0` → the `search` result is sent
   as `pay`).

## Goals

- The translator never sends two adjacent contents with the same role. It merges them into one
  content and keeps the order of the parts.
- The rule applies to every item, so replayed session items are also covered.
- Each `functionResponse` gets the name of the call that it closes, by the core pairing rule
  (below), within the limits in "Known limits".

## Non-goals

- Gemini thought signatures and `ContentPart::Reasoning` translation (still deferred, D3).
- Changes to the fallback id scheme in `stream.rs`. A unique fallback id would remove the collision
  for new histories, but stored sessions already hold repeated `fc_0` ids, so the translator must
  pair by order anyway. A follow-up Linear issue can track the id scheme.
- Changes to other providers.

## Approaches

1. **Merge after translation (picked).** Translate each item to `(role, parts)` as today, then fold
   adjacent entries with the same role into one content. One small post-pass covers every source
   of adjacency with one rule.
2. **Merge at each push site.** Each branch checks the last content and appends to it. This repeats
   the same check in five branches, and a new branch can forget it.
3. **Reorder items in core before the provider sees them.** Rejected: the problem is a Gemini wire
   rule, and other providers accept the core order as it is.

Sven approved approach 1 on 2026-11-20, and approved this spec at GATE 1 on 2026-11-20.

## Design

### Merge

- Build a `Vec<(role, Vec<Value>)>` in item order, then fold it: when the role of an entry equals
  the role of the last merged entry, append its parts; else start a new content.
- **Exception, the tool-result boundary** (added 2026-11-20 after the live run, Sven's decision):
  a `user` entry whose first part is not a `functionResponse` does not merge into a `user` content
  whose last part is a `functionResponse`. It starts a new `user` content. So a user text after a
  tool result stays a separate content, as on `main`.
  - Reason: Gemini 3 checks thought signatures only in the current turn, and the current turn
    starts at the last `user` content that is not a tool result. With the merge, `user[fr, text]`
    keeps the call in the current turn, and the translator sends no signature (SMA-827): HTTP 400
    (probe shape 3 with no signature; the first live run of this branch failed 3 of 3 this way).
    The split shape gets HTTP 200 with and without a signature (probe shape 2).
  - Consequence: the SMA-804 `Inject` shape stays two adjacent `user` contents. The probe shows
    that Gemini accepts it. Parallel results, adjacent user texts, and all `model` contents still
    merge. Review this exception when SMA-827 lands.
- An entry with no parts is dropped before the fold. Today only an empty `user` content is
  dropped. An assistant message with only `Reasoning` parts gives a `model` content with
  `"parts": []`.
- An empty `ContentPart::Text` (`text == ""`) in a user or assistant message is dropped (SMA-804
  spec §5.3 asks every translator to do this; SMA-819 did it for Anthropic). Without this, the
  merge puts `{"text": ""}` into the content with the `functionResponse`.
- `Item::System` stays in `systemInstruction` and does not take part in the merge. Items that the
  translator skips, and `System` items, do not break adjacency.
- Intended consequences:
  - Parallel calls: N adjacent `ToolCall` items become one `model` content with N `functionCall`
    parts, and N results become one `user` content with N `functionResponse` parts.
  - An assistant text followed by its `ToolCall` becomes one `model` content (text part, then
    `functionCall` part).
  - Two adjacent user texts become two `text` parts in one content. They are not joined.
- The order of parallel `functionCall` parts is the core item order. `build_items` in core emits
  calls in `BTreeMap<call_id, _>` order (`crates/paigasus-helikon-core/src/model.rs:514,530`), so
  `fc_10` comes before `fc_2`. This spec does not change that. Note for D3: Gemini 3 puts the
  thought signature on the first `functionCall` part.
- The empty-conversation error stays: if no content remains after the fold, return the error.

### Name resolution (core pairing rule, FIFO)

- One pass in item order. The pass keeps a list of open calls (call id, name), in order. The
  translator must not store names by call id in a pre-pass, because that brings the defect back.
  The open-call state goes `&mut` into `user_parts`, or the pass stores names by occurrence.
  - A `ToolCall` item, or a `ContentPart::ToolUse` in an `AssistantMessage`, adds an open call.
  - A result (`Item::ToolResult`, or `ContentPart::ToolResult` in a `UserMessage`) closes the
    **earliest** open call with its id and takes that name.
- This is the rule that core uses: `resume_exec::paired_result` (`resume_exec.rs:200-220`, "each
  result closes the earliest open call with its call id", "as in the session recorder") and
  `deferral::agent_view` (`deferral.rs:585-620`). `run_tools_concurrent` returns one outcome per
  call in call order (`agent.rs:748`). With FIFO, two calls in one round that share `fc_0` get the
  correct names. The cross-turn `search`/`pay` case also works.
  - **This changes the ticket text.** The ticket says "the most recent earlier call with the same
    id that has no result yet" (LIFO). LIFO gives both names wrong for two same-id calls in one
    round, where `main` gives one of two. Sven chose FIFO at GATE 1 on 2026-11-20.
- A result that closes no open call, but that has an earlier call with its id (a second result for
  one call), is **dropped** with a `tracing::warn!` (target `paigasus::gemini::translate`). This
  matches `agent_view`, which drops a result that closes no open call. It also prevents a merged
  `user` content with more `functionResponse` parts than its `functionCall` parts. (This replaces
  the earlier call "use the most recent earlier call", which could send two responses for one
  call.)
- A result whose id has no earlier call returns an error:
  - if a later item has a call with that id: a new message, `tool result for call_id {id} comes
    before its call`;
  - else the existing `tool result references unknown call_id {id}`.
- Behavior change: a result before its call now gives an error. Today the global map accepts it.
  No core producer makes this order: `close_open_transfers` inserts at `at + 1`
  (`resume_exec.rs:194`), the resume block follows the calls of the earlier run, and the legacy
  repair moves results after their calls. A stored session in this order now fails at `invoke`
  instead of being sent. The commit message names this error so that the CHANGELOG shows it.

### Invariant and known limits

The merge depends on one invariant: the next `user` content answers each `functionCall` of a
`model` content. Two real core histories do not hold it:

1. **Open deferred calls.** `RunDeferred` leaves calls with no result. `CompactingSession` sends
   `snapshot + prompt` to its model (`compacting_session.rs:200-211`). With a Gemini summarizer
   this gives, for example, `model[fc c1, fc c2], user[fr c1, text]`. Probe shape 14 shows what
   Gemini does with it. This spec does not add synthetic results; if shape 14 fails, a follow-up
   issue covers it.
2. **Replay of a `ParallelAgent` session.** Sibling events interleave through `select_all`
   (`workflow.rs:377`), and sibling Gemini agents all start at `fc_0`. The merge can put two
   `functionCall` parts with the same id into one content. No order rule can name those results
   correctly. This spec does not fix it.

## Tests

Unit tests in `request.rs` assert the full `contents` array, as
`tool_result_then_user_message_request_shape` does:

- `tool_result_then_user_message_request_shape` (and its doc comment): the assistant text and
  the call merge into one `model` content; the `functionResponse` content and the user text stay
  two `user` contents (the tool-result boundary). The ticket asked for one merged `user` content;
  Sven changed that on 2026-11-20 after the live run.
- The tool-result boundary: a user text after results stays separate; a result after a result
  still merges; a user text after a user text still merges.
- More than two adjacent contents with the same role merge into one, in order (a result, then two
  user messages).
- Two calls in different turns that share `fc_0` (`search`, then `pay`): correct name on each
  `functionResponse`.
- Two calls in one round that share `fc_0` (`search`, then `pay`), as two `ToolCall` items and as
  two `ToolUse` parts in one `AssistantMessage`: the first result is `search`, the second `pay`.
- Results in a different order than the calls (distinct ids): each gets its own name.
- A second result for one call is dropped.
- A result before its call gives the new error.
- A nested `ContentPart::ToolResult` mixed with `Text` in a `UserMessage`, directly after an
  `Item::ToolResult`: one merged `user` content with correct names.
- An `Item::System` between two user messages: one merged `user` content.
- A reasoning-only assistant message is dropped and its neighbours merge.
- An empty `Text` part is dropped.

Snapshots: update `snap_parallel_same_name_tool_calls` and its inline asserts in `mod.rs`, and any
other snapshot that the merge changes. Review each snapshot diff by hand.

Live test (mandatory, `#[ignore]` in `tests/live.rs`, needs `GEMINI_API_KEY`): call
`GeminiModel::invoke` with `search` and `pay` declared, for three histories: the SMA-804 shape
(result then user text), one parallel round, and a cross-turn `fc_0` collision. Assert no `Err`
event and a `Finish` event. This is the "after" check through the real translator.

## Live check (acceptance criterion)

The probe `probe.sh` sends hand-made request bodies to `gemini-2.5-flash` through the REST API
(`generateContent`), with `search` and `pay` declared. It prints the HTTP status, the finish reason
and a short part summary. It never prints request headers. The probe source goes into the commit
body. Shapes:

1. baseline: `user, model[fc], user[fr]`
2. SMA-804 today: `..., user[fr], user[text]`
3. SMA-804 merged: `..., user[fr, text]`
4. three adjacent `user` contents
5. parallel today: `model[fc0], model[fc1], user[fr0], user[fr1]`
6. parallel merged: `model[fc0, fc1], user[fr0, fr1]`
7. `functionResponse` name differs from its `functionCall` name
8. repeated `fc_0` across turns, correct names
9. repeated `fc_0` across turns, today's wrong name
10. `model[text, fc]`
11. parallel merged, responses in a different order than the calls
12. `user[fr, {"text": ""}]`
13. two `functionResponse` parts for one call
14. parallel merged, one call has no response, then text

Pass rule for a target shape (3, 6, 8, 10, 11): HTTP 200, `finishReason: STOP`, and an answer that
uses the tool result.

Order: run the probe **before** any code. If Gemini rejects shape 3 or 6, stop and bring it back to
Sven before implementation (fallback: keep the merge for `model` contents only, or put the text in a
separate content after a synthetic `model` turn; Sven decides).

### Results before the fix (2026-11-20, Developer API, hand-made bodies)

`gemini-2.5-flash` and `gemini-2.5-flash-lite`: HTTP 404 on every shape, "no longer available to
new users". The probe therefore ran on `gemini-3.8-flash`.

`gemini-3.8-flash` with no `thoughtSignature`: HTTP 400 "Function call is missing a
thought_signature in functionCall parts" on every shape in which the call is in the current turn
(1, 3, 5-14). Shapes 2 and 4 returned HTTP 200 / `STOP`, because the trailing user text moves the
call into an earlier turn, and Gemini checks signatures only in the current turn.

`gemini-3.8-flash` with the documented dummy signature `skip_thought_signature_validator` on each
`functionCall` part:

| Shape | Result |
|---|---|
| 1 baseline | 200 STOP, uses the result |
| 2 SMA-804 today (two adjacent `user`) | 200 STOP, uses the result |
| 3 SMA-804 merged | 200 STOP, uses the result |
| 4 three adjacent `user` | 200 STOP |
| 5 parallel today | 200 STOP |
| 6 parallel merged | 200 STOP |
| 7 response name differs from call name | **400 "Request contains an invalid argument."** |
| 8 repeated `fc_0`, correct names | 200 STOP |
| 9 repeated `fc_0`, today's wrong name | **400 "Request contains an invalid argument."** |
| 10 `model[text, fc]` | 200 STOP |
| 11 merged, responses reordered | 200 STOP |
| 12 `user[fr, {"text": ""}]` | 200 STOP |
| 13 two responses for one call | 200 STOP |
| 14 merged, one call with no response | 200 STOP, the model calls `search` again |

What this means:

- Defect 1 (adjacent same-role contents) causes no failure on `gemini-3.8-flash`. With a signature,
  both the split and the merged shapes are accepted. Without a signature, only the split shape is
  accepted (shapes 2 vs 3). So the merge keeps the tool-result boundary (see "Merge").
- Defect 2 (wrong name) is a **hard failure**: HTTP 400 on the next model call. Part 2 is the fix
  that matters for users.
- Gemini accepts a second response for one call (13). The translator still drops it, to match core.
- Shape 14 (an open call with no response) is accepted. Known limit 1 causes no API error.

### Out of scope, found by the probe

The Gemini provider does not read or send `thoughtSignature` (no match in `crates/`). With a
current Gemini model, every request in which the current turn holds a `functionCall` gets HTTP 400.
So tool loops on Gemini 3.x fail at the second model call. SMA-827 tracks this. The model ids in
the crate docs and in `tests/live.rs` (`gemini-2.5-flash`) are not available to new users; SMA-565
tracks them. This PR changes only the `tests/live.rs` default (below); the other occurrences stay
in SMA-565.

### After the fix

The live test in `tests/live.rs` runs through the real translator. Because the translator sends no
thought signatures, each history in the test ends with a user text after the last tool result. With
the tool-result boundary, that text is a separate `user` content, so no call is in the current turn.
(The first version of this section assumed this without the boundary. The first live run proved it
wrong: 3 of 3 tests got HTTP 400 for a missing thought signature.) Shape 9 shows that Gemini checks names in earlier turns too, so the
cross-turn `fc_0` history still tests the name fix. The test model defaults to `gemini-3.8-flash`
(override with `GEMINI_MODEL_ID`); the default of the existing `dev_model()` changes from
`gemini-2.5-flash` to `gemini-3.8-flash`, because the old default returns 404 for new users.

Results after the fix (2026-11-20, `gemini-3.8-flash`, `cargo test -p paigasus-helikon-providers-gemini
--test live -- --ignored live_developer_`):

| Live test | This branch | Control: same test file on `main` (e7ac5377) |
|---|---|---|
| `live_developer_text_turn` | pass | pass |
| `live_developer_structured_output` | pass | pass |
| `live_developer_tool_result_then_user_text` | pass | pass |
| `live_developer_parallel_round` | pass | pass |
| `live_developer_cross_turn_repeated_call_id` | pass | **fail: HTTP 400 "Request contains an invalid argument."** |

The first run of this branch, before the tool-result boundary, failed the three tool tests with
HTTP 400 "Function call is missing a thought_signature".

Scope of the check: Developer API only. Vertex uses the same translator.

## Docs and release

- No book or README page describes the old shape, so no book or README edit is needed.
- release-plz bumps `paigasus-helikon-providers-gemini` (patch) and the facade by cascade. No hand
  version bump.

## Left to the builder

- Helper names, and how the open-call state reaches `user_parts`, within the one-pass rule above.
