# SMA-585: The Responses API sends no `data: [DONE]` (design)

## Intent

The OpenAI Responses fixtures must not claim more than the wire sent.
`responses_tool_call.txt` is labelled CAPTURED and calls itself "a literal
transcription of a real capture". Its header lists every trim, but the file
has two edits that the header does not record:

1. It ends with `data: [DONE]`, which no capture of this endpoint contains.
2. Its JSON keys are in a different order from the wire. The order matches
   the hand-authored sibling fixtures (see Appendix A).

The root cause is wider than "the header has no place for additions". The
transcription was made to look like its siblings, and the header structure
could record only removals. These fixtures are load-bearing. SMA-522 went
wrong because a fixture was derived and not captured. A CAPTURED label that
overstates its file erodes the reason the label exists.

## Evidence

- **2026-08-22** (from SMA-562): four `curl` captures against
  `/v1/responses` (`gpt-4o-mini`, `gpt-5-mini`, a `background: true`
  request, a resumed stream). None contains `[DONE]`.
  `responses_tool_call_zero_args.txt` is one of them, byte-faithful.
- **2026-11-21** (this ticket): four more captures. The script ran with
  `set -euo pipefail`, so a curl error or a `--max-time` stop would have
  ended it. All four completed, so each curl exited 0 when the server closed
  the stream. Each file ends on its terminal event followed by `\n\n`, and
  none contains the string `DONE`. Appendix A records the details.
- No raw copy of the 2026-08-20 capture exists in the repo or in any spec or
  plan. What the endpoint sent on that date cannot be proven. Two later
  captures on two dates speak against "OpenAI sent it then stopped".
- The other four `responses_*.txt` fixtures have no provenance header. The
  SMA-316 spec (`2026-05-26-sma-316-openai-provider-design.md`, line 365)
  says they are "Hand-authored". Their `[DONE]` is framing copied from the
  Chat Completions shape.
- `[DONE]` and end of body are equivalent for every Responses terminal event.
  This follows from the code, not only from the SMA-562 probe:
  - async-openai 0.43.0 `src/client.rs:846-873`: on `[DONE]` the loop does
    `break`; on end of body the loop ends. Both drop the sender.
  - `src/backend/responses.rs:71`: the driver returns on `None`.
  - `src/backend/responses.rs:695-702`: `response.failed` yields `Err`, and
    the driver returns before it reads more.
- The repo has no first-party capture of Chat Completions. The
  `chat_text_usage_trailing.txt` capture is from a LiteLLM proxy. This spec
  does not depend on what Chat Completions sends.

## Goals

1. No fixture or test body presents `[DONE]` as OpenAI Responses wire
   behaviour.
2. Every Responses fixture header states its provenance and records every
   edit from its source.
3. A contributor rule in `CONTRIBUTING.md` records additions and changes as
   well as removals, and covers hand-authored files and inline wire helpers.
4. Every cross-reference to these fixtures in code and fixture headers stays
   true after the edit. Dated design documents under `docs/superpowers/**`
   are records of their time and are not edited.
5. Tolerance of a trailing `[DONE]` stays covered by one labelled test,
   because `base_url` is advertised for OpenAI-compatible servers
   (`src/builder.rs:72`).

## Non-goals

- Chat Completions fixtures and tests (`chat_*.txt`, `cancellation.rs`,
  `chat_wire.rs`).
- Replacing the fixtures with the new captures (approach B, rejected below).
- Restoring the wire key order in `responses_tool_call.txt`. The 2026-08-20
  order is unknown; a reorder to the 2026-11-21 order would be a second
  reconstruction from a different capture. The header records the change
  instead.
- Auditing fixtures in other crates. Files that do not follow the new rule
  on merge day go to a Linear follow-up (see Open items).
- Production code. `ResponsesTranslator` handles both shapes.

## Anti-goals

- A test that passes only because of `[DONE]`. If a test changes behaviour
  when the sentinel goes, stop and investigate. Do not add the line back.
- Changing any asserted id, model or token count.

## Approaches

- **A. Correct the existing files (chosen).** Keep the 2026-08-20
  transcription and the current hand-authored values. Remove the sentinel,
  record every edit in the headers, and update the cross-references. No test
  assertion changes.
- **B. Replace with the new captures (rejected).** The tests and the
  conformance suite assert on the current ids, models and token counts, so
  many assertions must change. `response.failed` and a content-filter
  refusal cannot be triggered on demand, so two fixtures stay hand-authored
  anyway. The diff is too large for a Low-priority ticket.
- **C. Add notes and keep `[DONE]` (rejected).** A CAPTURED file would still
  carry a line that the wire does not send.

Sven picked A on 2026-11-21.

## Design

Paths are relative to `crates/paigasus-helikon-providers-openai/` unless
stated otherwise.

### 1. `tests/fixtures/responses_tool_call.txt`

- Remove the trailing `data: [DONE]` line.
- Remove the word "literal" from the `event:` paragraph, and change "unlike
  this directory's other `responses_*.txt` fixtures" so that it also names
  `responses_tool_call_zero_args.txt`, which keeps `event:` lines too.
- Replace "Three kinds of trimming from the raw capture" with "Edits from
  the raw capture", in three fixed parts:
  - **Removed:** the three existing trims, with their text kept.
  - **Added:** nothing.
  - **Changed:** the JSON key order. The file puts `type` and
    `sequence_number` first and `item.type` before `item.id`, to match the
    hand-authored siblings. The 2026-08-22 and 2026-11-21 captures put
    `type` first and the other event keys in alphabetical order, and
    `item.id` before `item.type`. The 2026-08-20 order is not on record.
    The parser does not depend on key order.
- Add a dated correction note: until 2026-11-21 this file ended with a
  `[DONE]` sentinel line and had a changed key order, and the header
  recorded neither. The note gives the evidence in short form and points to
  this spec. Write the note so that no header line, with its `:` removed,
  is equal to `data: [DONE]`.

### 2. The four hand-authored fixtures

Remove the trailing `data: [DONE]`. Add a `:` comment header per file:

- All four: HAND-AUTHORED under SMA-316, not captured. The event shapes
  follow the async-openai `ResponseStreamEvent` types.
- `responses_reasoning_then_text.txt` and `responses_incomplete_length.txt`:
  the stream ends on the terminal event with no sentinel, as the 2026-11-21
  captures of `response.completed` (reasoning, `gpt-5-mini`) and
  `response.incomplete` (`max_output_tokens`) show.
- `responses_failed.txt` and `responses_incomplete_filter.txt`: no capture
  of this terminal event exists. The sentinel is omitted because no
  captured Responses stream ends with it.

### 3. Responses test bodies

**a. `tests/provider-stream-conformance/tests/conformance.rs`, module
`openai_responses`:**

- Remove `done()` and all three uses: `CleanStop` (line 3046),
  `ToolCallCleanStop` (3112), and the `("[DONE]", done())` row in
  `scan_finds_only_the_terminal_event` (3266). Delete that row.
- Correct the doc comments that cite `[DONE]`: the module doc "Fixture
  provenance" (2804-2816), which must also say that
  `responses_reasoning_then_text.txt` is HAND-AUTHORED, and the `CleanStop`
  arm comment.
- In the `CleanStop` arm comment, say that `response.completed` is both the
  stop reason and the terminator, and that the wire has no separate
  sentinel. This keeps the scenario consistent with the `CleanStop`
  definition in `tests/provider-stream-conformance/src/lib.rs:38-41`. The
  decline text for `TruncatedAfterStopReason` does not change.
- Replace the line citations into `responses_tool_call.txt` (2954, 2974,
  2989, 3004) with `sequence_number` citations, which do not move when the
  header changes: `output_item.added` is 2, the argument deltas are 3-7,
  `function_call_arguments.done` is 8, `output_item.done` is 9,
  `response.completed` is 10.
- Correct "four of the five `responses_*.txt` fixtures" (2890-2893). There
  are six, and two keep `event:` lines.

**b. `tests/responses_wire.rs`:**

- The `sse()` helper stops appending `data: [DONE]`. Correct its doc
  comment.
- Add one test, `trailing_done_sentinel_is_tolerated`. It sends a text
  delta, `response.completed`, and then `data: [DONE]`, and asserts the
  same events as the same stream without the sentinel. Its doc comment says
  that this is a compatibility check for OpenAI-compatible servers, not a
  claim about OpenAI's wire.

**c. `tests/responses_streaming.rs`:**

- Add one guard test that asserts no `responses_*.txt` fixture has a line
  equal to `data: [DONE]`. Behaviour tests cannot detect a re-added
  sentinel, because the two endings give identical events.

### 4. `tests/fixtures/responses_tool_call_zero_args.txt`

- Rename the "Two deliberate edits" list to the Removed / Added / Changed
  structure. Changed: nothing.
- Change "Unlike every sibling fixture here, this file does NOT end with
  `data: [DONE]`" (lines 48-50), which becomes false.
- Change the paragraph that calls `responses_tool_call.txt`'s `[DONE]`
  "unexplained" and "a follow-up" (lines 56-58) so that it points to the
  SMA-585 resolution.

### 5. `CONTRIBUTING.md` and `CLAUDE.md`

Add a "Wire-format fixtures" section to `CONTRIBUTING.md`:

- **Label.** A fixture states its provenance: CAPTURED (endpoint, model,
  date), HAND-AUTHORED, or CAPTURED with named reconstructed parts.
- **Edits.** A CAPTURED fixture lists every edit from the raw capture in
  three parts: Removed, Added, Changed (reorder, reformat, value edits).
  When a part is empty, it says "nothing".
- **Hand-authored shapes.** A HAND-AUTHORED fixture names the source of
  each shape: a capture or a vendored type. Do not copy framing from a
  different endpoint or API.
- **No conformity edits.** Do not add, remove or reorder anything to make a
  fixture match a sibling. If you must, record it under Changed.
- **Where the header goes.** SSE and other text fixtures carry a `:`
  comment header. A format with no comment syntax records its provenance in
  the doc comment of the test that loads it. Inline wire helpers in test
  code cite their source the same way. A synthetic addition in a test
  script is allowed when its doc comment labels it as synthetic (for
  example the Gemini `done()` in `conformance.rs`).

Add one line under "Fixture line endings" in `CLAUDE.md` that points to the
new section. It is a pointer, not a copy.

## Verification

- `cargo test -p paigasus-helikon-providers-openai --all-features` and
  `cargo test -p paigasus-helikon-provider-stream-conformance` pass. No
  existing assertion changes. The two new tests pass.
- Before the `[DONE]` removal, run the guard test from 3c and see it fail.
  After the removal, see it pass.
- Each `sequence_number` citation in `conformance.rs` names the event that
  carries that number in `responses_tool_call.txt`.
- The CI gates in `CLAUDE.md` pass (`fmt`, `clippy`, `docs`,
  `markdown-lint`).

## Rollout

`crates/paigasus-helikon-providers-openai/Cargo.toml` has no `include` or
`exclude`, so `tests/` ships in the published package. release-plz will
give `paigasus-helikon-providers-openai` a patch bump and cascade to the
facade, for a test-only change. This is expected and harmless. Commit
scopes: `test(providers-openai)` for the crate, `test(providers)` for the
conformance suite, `docs(contributing)`, `docs(claude)`, `docs(spec)`.

No book or README edit. The change is internal to tests, fixtures and
contributor policy, and it changes no public API.

## Open items

- File a Linear follow-up for fixtures in other crates that do not follow
  the new rule on merge day. Known examples: the LiteLLM CAPTURED fixtures
  (`tool_call_stream.txt:1-9` lists no edits) and the mixed
  `chat_text_usage_trailing.txt`.

## Left to the builder

- The exact wording of the headers and of the `CONTRIBUTING.md` section.
- Where in `CONTRIBUTING.md` the section goes.

## Appendix A: 2026-11-21 captures

Endpoint `https://api.openai.com/v1/responses`, captured 2026-11-21
16:58:54Z to 16:59:05Z with `curl -sS -N --max-time 120`. Every response:
`HTTP/2 200`, `content-type: text/event-stream; charset=utf-8`,
`openai-version: 2020-10-01`. Organization and project headers are not
recorded.

| Capture | Model | Last event | Trailing bytes | `DONE` count |
|---|---|---|---|---|
| tool call, `get_weather` | `gpt-4o-mini` | `response.completed`, `sequence_number` 10 | `…"sequence_number":10}\n\n<EOF>` | 0 |
| plain text | `gpt-4o-mini` | `response.completed`, `sequence_number` 13 | `…"sequence_number":13}\n\n<EOF>` | 0 |
| reasoning, `summary: auto` | `gpt-5-mini` | `response.completed`, `sequence_number` 84 | `…"sequence_number":84}\n\n<EOF>` | 0 |
| `max_output_tokens: 16` | `gpt-4o-mini` | `response.incomplete`, `sequence_number` 23 | `…"sequence_number":23}\n\n<EOF>` | 0 |

Key order of the tool-call capture, against the fixture:

| Event | 2026-11-21 capture | `responses_tool_call.txt` |
|---|---|---|
| `output_item.added` / `.done` | `type,item,output_index,sequence_number`; item `id,type,status,arguments,call_id,name` | `type,sequence_number,output_index,item`; item `type,id,status,arguments,call_id,name` |
| `function_call_arguments.delta` | `type,delta,item_id,obfuscation,output_index,sequence_number` | `type,sequence_number,item_id,output_index,delta` |
| `function_call_arguments.done` | `type,arguments,item_id,output_index,sequence_number` | `type,sequence_number,item_id,output_index,arguments` |
| `response.completed` | `type,response,sequence_number`; response `id,object,created_at,status,…,model,…,output,…,usage,user,metadata` (38 keys) | `type,sequence_number,response`; response `id,object,created_at,model,output,status,incomplete_details,usage` |

The capture's event sequence is: `response.created`, `response.in_progress`,
`output_item.added`, five `function_call_arguments.delta`,
`function_call_arguments.done`, `output_item.done`, `response.completed`.
This is the same sequence as the fixture plus the two dropped opening
events.
