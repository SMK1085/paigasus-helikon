# SMA-845: wire-format fixture provenance audit

Linear: [SMA-845](https://linear.app/smaschek/issue/SMA-845/audit-wire-format-fixture-headers-in-other-crates-against-the-new).
Rule: `CONTRIBUTING.md` → "Wire-format fixtures" (added by SMA-585, PR #381).

## Intent

SMA-585 added a provenance rule for wire-format fixtures and made the OpenAI
Responses fixtures obey it. The fixtures and wire helpers in the other crates
do not obey it yet. A reader of those files cannot tell which bytes a provider
really sent. This change makes each in-scope fixture and wire helper state its
provenance under the rule.

## Goals

1. Each in-scope fixture file carries a provenance header that obeys rules 1-5.
2. Each in-scope JSON fixture has its provenance in the doc comment of the test
   that loads it (rule 5).
3. Each in-scope inline wire helper cites its source in its doc comment (rule 5).
4. Each statement in a header is true to the record. Where the record is silent,
   the header says so. Where the record contradicts the new label, the header
   cites the contrary line and the evidence for the label (D11).

## Scope

**In scope:** bytes that a model-provider HTTP API sends, where a test serves
them in place of that API, plus the fixture files that the ticket lists
(including the Brave and Tavily JSON in `paigasus-helikon-tools`). This means:

- the fixture files under `crates/*/tests/fixtures/` (D3-D8);
- the wire helpers in `tests/provider-stream-conformance/tests/conformance.rs`
  (D9);
- the HTTP-served wire helpers and literals in the provider integration tests
  (D10).

Sven chose this scope (option 2 of 3) on 2026-11-21.

**Out of scope:**

- The `#[cfg(test)]` unit tests that feed `json!` values, raw strings or typed
  chunks into a parser or translator without HTTP (for example
  `litellm/src/sse.rs`, `litellm/src/stream.rs`, `gemini/src/stream.rs`,
  `openai/src/backend/chat.rs`, `bedrock/src/stream.rs`).
- The forkd controller JSON in `paigasus-helikon-tools/tests/forkd_*.rs`. forkd
  is not a model provider and the ticket does not list it.
- The plain-text and HTML bodies in `paigasus-helikon-tools/tests/web_fetch.rs`
  and `tests/fixtures/article.html`. They are extractor input, not provider
  bytes.
- New captures. This work uses only the record that exists today.
- Translator or production code.
- mdBook and crate README changes. This is test-only and contributor-doc work
  with no effect on the public surface.

## Anti-goals

- A header that says "nothing" for a part that nobody can check. That is the
  defect SMA-585 found, in a new form.
- A header that names a source (a doc page, a capture, a value) that the record
  does not show. This design itself had one such value in its first draft (a
  full response id that no record contains). The builder checks each value
  against the record.
- A data edit made only to make a fixture look like a sibling (rule 4).

## Constraints

### Header safety, loader by loader

A `:` header goes at the top of each SSE or `.txt` fixture. Each loader that
reads these files was checked:

| Loader | Files | Why a `:` header is safe |
|---|---|---|
| `eventsource-stream` 0.2.3 via wiremock (Anthropic, LiteLLM, Gemini, OpenAI Chat through async-openai) | all SSE fixtures | Comment lines are ignored (`event_stream.rs:70`). A header followed by a blank line dispatches nothing (`:100`). |
| `anthropic/tests/messages_streaming.rs:201-203` | `tool_use_then_continuation.txt` | Splits on `# --- turn 2 ---\n`. The header lands in part 1. The header must not contain the delimiter string. |
| `gemini/src/translate/mod.rs:355-358` | `parallel_signed.sse` | Skips each line that does not start with `data: `. |
| SMA-840 `anthropic/src/stream.rs` `translate_fixture` and `src/translate/mod.rs` `fixture_events` | Anthropic fixtures | Skip each line that does not start with `data: `. |
| **SMA-840 `anthropic/src/translate/mod.rs` `no_thinking_turn_writes_no_provider_data`** | `tool_use_then_continuation.txt` | **Not safe.** It starts a new part at the first line and at each `event: message_start`, then asserts two parts. A header makes three. Phase B changes this test so that it skips lines that start with `:` (a test-only edit). |

### Byte-level traps

- `body_cut_inside_message_stop.txt` ends inside an event with no final
  newline, on purpose. The header goes at the top. The last byte must stay
  something other than `0x0a`. No test catches an added newline, because the
  parser drops the incomplete event at EOF in both cases.
- A header line without its leading `:` passes every test, because the parser
  ignores unknown fields (`event_stream.rs:67`). The verification checks this.
- `.gitattributes` pins these fixture directories to `text eol=lf`. Do not
  change that.

### Merge order and phases

The Anthropic work depends on SMA-840, which is not merged (local branch
`feature/sma-840-anthropic-provider-drops-thinking-signatures`, head
`9f3a84f3`, no PR on 2026-11-21). SMA-840 adds Anthropic captures, edits the
Anthropic conformance module and adds a loader that a header breaks. So the
work runs in two phases inside one PR:

- **Phase A (now, on `main`):** LiteLLM, OpenAI Chat, Gemini, tools,
  `CONTRIBUTING.md`, the non-Anthropic conformance modules and the
  non-Anthropic D10 items.
- **Phase B (after SMA-840 merges, after a rebase onto `main`):** the nine
  Anthropic fixtures, the Anthropic conformance module and its new SMA-840
  helpers, the Anthropic D10 items, the loader edit above, and the five
  SMA-840 fixture headers (Q2).

The PR stays a draft until phase B is done. That stops it from merging before
SMA-840.

### Commits

Use `test(providers)`, `test(tools)`, `docs(contributing)`, `docs(specs)` and
`docs(plan)`. No `feat` or `fix`. Each touched crate still gets a release-plz
patch bump, because the changed files are in packaged crates. That is accepted.
Cite PRs and tickets, not commit hashes: squash merges do not keep branch
commits on `main`.

## Approaches

1. **One PR, merged after SMA-840 (chosen).** Phase A now, phase B after
   SMA-840 merges. The PR body states the merge order. Cost: a merge-order
   dependency, and the PR is open for longer.
2. **Two PRs.** Phase A merges now. Phase B is a second PR. No dependency, but
   two review loops.
3. **Wait for SMA-840.** Simple, but it stops all the work for one fixture
   group.

Sven chose approach 1 on 2026-11-21. The phase split came from the spec
challenge. It keeps approach 1 and does not add a PR.

## Decisions

### D1. Header form

Use the form of the worked example,
`crates/paigasus-helikon-providers-openai/tests/fixtures/responses_tool_call.txt`,
and of `responses_reasoning_then_text.txt` for a HAND-AUTHORED file. Keep the
header short when the record is short. A header contains, in this order:

1. `Provenance:` and one of the three labels.
   - CAPTURED: the endpoint (or proxy and image), the model, the date, and the
     ticket or PR.
   - HAND-AUTHORED: the ticket or PR, and the source of each shape.
   - CAPTURED with named reconstructed parts: the capture data, then a list of
     the parts that are reconstructed.
2. For both CAPTURED labels: `Edits from the raw capture:` and the three parts
   Removed, Added and Changed.
3. Any line in the data that the wire did not send, with the reason.
4. Optional: a comparison with a later capture (D3). It is labelled as a
   comparison, never as a source.
5. Optional: a correction note for a contrary record (D11).

A date or model that is inferred, not recorded, is written as "not recorded"
and followed by the evidence ("`created` 1786999072 decodes to 2026-08-17").

### D2. "nothing" and "not recorded" (Sven approved: Q1)

Rule 2 requires "nothing" for an empty part. It does not cover a part that
nobody can check, because no raw capture exists. This design uses this reading:

- "nothing" only for a part that the record shows is empty, with the evidence
  named;
- "not recorded" for a part that nobody can check, with the evidence that does
  exist.

This reading needs no rule change, because rule 2 asks for "nothing" only for a
known-empty part. The proposal is to state it in one sentence in rule 2:

> Write "nothing" only when the record shows that a part is empty. If the raw
> capture is not available, write "not recorded" for a part that you cannot
> check, and say what evidence exists.

The worked example `responses_tool_call.txt` says "Added: nothing" (line 81) and
also "No raw copy of the 2026-08-20 capture exists" (line 99). With the sentence
in place, the worked example would break the rule that cites it. Q1 asks Sven
whether to add the sentence and whether to correct the worked example in this
PR.

### D3. Anthropic fixtures (phase B)

Sven chose to compare the hand-authored shapes with the SMA-840 captures. Each
of the nine Anthropic fixtures is HAND-AUTHORED. Each header has two parts:

1. **Source.** "Source of the shapes: not recorded. The SMA-317 design records
   only 'Hand-authored — they double as wire-format documentation'
   (SMA-317 design, line 465)." The SMA-840 captures are six months younger, so
   they cannot be the source.
2. **Comparison.** "Later comparison with the SMA-840 transcriptions
   (2026-11-21)", then the differences.

The comparison:

- **Comparison set.** The SMA-840 files whose header says "transcribed from" a
  probe run: `empty_thinking_tool_use.txt` (run A, `claude-sonnet-5-5`),
  `thinking_text_tool_use.txt` (run C, `claude-sonnet-4-5-20250929`) and
  `redacted_thinking_tool_use.txt` (run C). Exclude the third from the key
  comparison, because its header records structural edits (blocks removed,
  indexes changed). Do not use the files whose header says "synthetic". Cite
  each by its final path on `main` and SMA-840, and check the path after the
  rebase. The comparison inherits the SMA-840 edit lists, which nobody has
  checked.
- **Method.** For each event type in the fixture, diff every key in both
  directions against the same event type in the comparison set. Record key
  order differences. The list below is a minimum, not the full result.
  - The captures send `ping`. No SMA-317 fixture has one.
  - `message_start`: the captures carry `container`, `stop_sequence`,
    `stop_details`, `cache_creation`, `service_tier`, `inference_geo`, `speed`
    and `diagnostics`. They put `model` first, and
    `cache_creation_input_tokens` before `cache_read_input_tokens`. Their
    `output_tokens` is not 0.
  - Thinking `content_block_start`: the captures carry `"signature":""`.
  - tool_use `content_block_start`: the captures carry
    `"caller":{"type":"direct"}`.
  - Tool input: the captures send a first `"partial_json":""` delta, then
    several fragments. The fixtures send one whole delta.
  - `message_delta`: the captures carry `stop_details` and `container` in
    `delta`, and `input_tokens`, `cache_creation_input_tokens`,
    `cache_read_input_tokens` and `output_tokens_details` in `usage`. The
    fixtures carry only `usage.output_tokens`.
  - Placeholders: ids (`msg_01`, `tu_a`), model `claude-sonnet-4-6`, signature
    `abc123`.
- **Shapes with no capture.** For a shape that no comparison file contains,
  write "no capture". Known cases: the `error` event, `stop_reason:"end_turn"`,
  two tool_use blocks in sequence, a turn without thinking. Do not cite the
  Anthropic docs: the record does not show that anyone used them.
- **The SMA-531 files.** `eof_after_message_delta.txt`,
  `body_cut_inside_message_stop.txt` and `eof_mid_content_block.txt` are cut
  copies of `text_only.txt`. Their headers say so and point to the
  `text_only.txt` header for the comparison. `error_after_message_delta.txt` is
  its own file (SMA-531 plan, lines 587-593): id `msg_06`, one "Hello" delta.
  Its `error` event is byte-identical to the one in `stream_error.txt`. Do not
  call `stream_error.txt` its source.
- **`tool_use_then_continuation.txt`.** The header says that the file holds two
  separate response bodies and that `# --- turn 2 ---` is a test delimiter, not
  wire bytes.

### D4. Gemini `parallel_signed.sse` (phase A)

Sven does not know whether the probe output was edited beyond the signature.
The header says:

- CAPTURED from the Gemini Developer API, streamed, `gemini-3.8-flash`,
  2026-11-20, SMA-827 (PR #373), capture 1 (three parallel calls). The exact
  URL is not recorded.
- Removed: not recorded. The SMA-827 design says the stream has four SSE chunks
  (lines 299-300), and the fixture has four. So no whole chunk was removed.
- Added: not recorded.
- Changed: the real 1140-character `thoughtSignature` is replaced with `SIG_A`
  (SMA-827 design, lines 289-291). `"thoughtSignature":"SIG_A"` is the only key
  with no space after its colon. Every other key has `": "`. So that reformat is
  probably part of the same edit.
- One sign is unexplained: `candidatesTokenCount` rises in equal steps (14, 28,
  42, 42). The header states it, so a reader does not read the values as proof
  of wire behaviour.
- The stream ends at EOF with no `[DONE]`. This matches the other Gemini
  capture (`conformance.rs`, Live capture section).
- It is a different capture from the `conformance.rs` capture
  (`gemini-3.6-flash`, 2026-08-20). Do not cite one as the source of the other.

### D5. LiteLLM fixtures (phase A)

The SMA-451 P1 trace (SMA-451 design, lines 1136-1144) replaces the start of
each line with `{…`. It records each line only from `"choices"` onward. So it
does not record the values or the order of `id`, `object`, `created` and
`model`.

| File | Label | Header content |
|---|---|---|
| `text_then_trailing_usage.txt` | CAPTURED with named reconstructed parts | Capture: LiteLLM 1.97.0, `ghcr.io/berriai/litellm:main-stable`, digest `sha256:468c25f35f3e5ec4e414974f00deab93337b1b4d9953cabcfd3722e59415f834`, keyless `mock_response`, model `mock-fast`, SMA-451 (PR #199). Date: not recorded. In the trace, from `"choices"` onward: the first content delta (`"role":"assistant","content":"Hel"`), the finish chunk, the usage chunk and `[DONE]`. Reconstructed: the second delta ("lo"). The mock text was "Hello from the mock backend.", but the fixture spells "Hello". The usage counts (6 completion tokens) belong to the full text, not to the fixture text. Not recorded: the `id`, `object`, `created` and `model` values and their order (the trace elides them). Changed: the id `chatcmpl-1` is a placeholder. |
| `truncated_no_finish.txt` | HAND-AUTHORED | Envelope from `text_then_trailing_usage.txt`. Ends with no finish chunk and no `[DONE]` on purpose (premature end). |
| `unknown_finish_reason.txt` | HAND-AUTHORED | Envelope from `text_then_trailing_usage.txt`. No mock can produce `guardrail_intervened`, and the value has no recorded source. The `[DONE]` matches the captured LiteLLM framing. |
| `unparseable_frame.txt` | HAND-AUTHORED | Synthetic by design. The line `data: {this is not json` is not wire bytes. |
| `tool_call_stream.txt` | CAPTURED (keep the header) | Add: date not recorded (`created` 1786999072 decodes to 2026-08-17; the SMA-547 design has the same date). The fake upstream was an uncommitted, hand-written frame list (SMA-547 design, lines 105-107). So the ids, the name split and the argument split are authored input. Only LiteLLM's serialization is captured. Removed, Added and Changed: not recorded. The SMA-547 "Captured frames" section (lines 109-115) records only the fragmented shape, and the config (lines 76-81) defines only `shape-fragment`. |
| `tool_call_stream_fragmented_name.txt` | CAPTURED (keep the header) | The same date and fake-upstream notes. For the `tool_calls` arrays: Removed, Added and Changed: nothing (checked against SMA-547 design lines 112-114). For the rest of each chunk: not recorded. LiteLLM's key reorder and the added `"type":"function"` are proxy output, not edits (lines 129-131). |

### D6. OpenAI Chat fixtures (phase A)

| File | Header change |
|---|---|
| `chat_text_usage_trailing.txt` | Relabel as CAPTURED with named reconstructed parts. Proxy: `ghcr.io/berriai/litellm:main-stable` (digest and version: not recorded), keyless `mock_response`, model `mock-fast` (configured as `openai/gpt-4o-mini`; SMA-522 design lines 441-442 and 483-488), 2026-08-16, SMA-522 (PR #197). In the trace (SMA-522 design A.1, lines 449-455): the first content delta ("Hel") and the last (".") with the id elided, the finish chunk, the usage chunk and `[DONE]`. Reconstructed: the middle content deltas. The number of elided deltas is not recorded. Edits: Removed: not recorded (the trace is abridged). Added: the reconstructed middle deltas. Changed: the id is shortened. The trace shows only the prefix `chatcmpl-1cc5e8c0-`. The full id is not recorded. |
| `chat_text_usage_trailing_empty_choices.txt` | HAND-AUTHORED (SMA-522, PR #197). The chunk sequence and `[DONE]` follow `chat_text_usage_trailing.txt`. `"choices":[]` comes from the async-openai field doc, quoted as today. Weaken "the shape api.openai.com emits" to what the doc says ("can also be empty"). |
| `chat_content_filter.txt` | HAND-AUTHORED (SMA-316, PR #30). The chunk sequence follows the capture, but the envelope values and key order do not (the usage chunk has `id,object,created,model`; the capture has `id,created,model,object`; no `completion_tokens_details`). SMA-522 (PR #197; SMA-522 design lines 267-270) split the usage chunk out of the finish chunk. The usage chunk keeps the proxy form `choices:[{"index":0,"delta":{}}]`, not `choices:[]`. The `content_filter` value has no capture. |
| `chat_parallel_tool_calls.txt` | The same as `chat_content_filter.txt`. The tool-call delta shape: no capture (write "not recorded" unless the builder finds a record). |

### D7. Tools fixtures (phase A)

- `brave_search.json`: a `///` doc comment on `parses_brave_fixture`
  (`src/web/backends/brave.rs`). HAND-AUTHORED under SMA-412 (PR #76). The
  shape is the subset that the SMA-412 design (lines 307-310) maps:
  `web.results[].{title,url,description}`. Content is invented. No capture and
  no vendor doc link is recorded. The real response has more fields; their
  absence here does not mean absence on the wire. `parse_brave_respects_limit`
  gets a one-line doc comment that points to `parses_brave_fixture`.
- `tavily_search.json`: the same on `parses_tavily_fixture`
  (`src/web/backends/tavily.rs`), SMA-412 design lines 312-318. Also name the
  top-level `query` key, which the mapping does not read (source not
  recorded). The URLs use `example.com`.
- The SMA-412 design (line 406) calls these JSON files "captured". Apply D11.

### D8. CLI fixtures

`agents.toml`, `tools/lookup_spending.rhai`, `triage.jsonl` and
`triage_script.json` are not wire-format fixtures. They are config, a tool
script, an eval dataset and the SDK's own script format
(`crates/paigasus-helikon-evals/src/script.rs`). No header. The PR body says
so, because the ticket counts them.

### D9. Conformance suite (`tests/provider-stream-conformance/tests/conformance.rs`)

The module-level "Fixture provenance" sections stay the main provenance layer.
The work:

1. **Labels.** Each module "Fixture provenance" section gives the label of each
   source it cites, as the fixture headers now state it.
2. **Wire-claim sweep.** In each module, find each claim about real or observed
   wire behaviour. Check it against a capture. If no capture supports it,
   reword it to say what the record shows, or remove it. Known claims:
   - line 586 (`openai_chat`), 1116 (`litellm`) and 2799 (`openai_responses`):
     "actually observed to send";
   - 616: "OpenAI sends `"finish_reason":null` on every ordinary content or
     tool-call delta";
   - 1148: LiteLLM backends "can still emit" an explicit null;
   - 1065: the shape that `unknown_finish_reason.txt` "captures";
   - 638 and 1073-1074: they depend on `chat_wire.rs` being "captured";
   - 1633: "Anthropic's real error mechanism" (no capture has an `error`
     event);
   - 2198 ("Gemini has no fixture directory") and 2217 (no other
     `functionCall` capture): both false since SMA-827 added
     `parallel_signed.sse`;
   - 2283-2285: a hand-authored inline body cited as evidence of "real Gemini
     traffic";
   - 1689: "no committed capture grounds" a split `input_json_delta`. This
     becomes false when SMA-840 merges (phase B).
   The line numbers are from `main` on 2026-11-21. The sweep in the Anthropic
   module runs in phase B, on the SMA-840 version.
3. **`openai_chat`.** State that each source is a LiteLLM-proxy capture or a
   hand-authored file, and that no `api.openai.com` Chat capture exists.
   `text_delta` sends `"finish_reason":null` on each delta. No capture sends
   it: the LiteLLM captures omit the key. Record that, and keep the helper data
   unchanged (non-goal: no data change without a check with Sven). The source
   it cites today, the `chat_wire.rs` body, is HAND-AUTHORED (D10).
4. **Changed items.** Each helper that changes its source records the change in
   its doc comment, in one line. Known items:
   - the `text_delta` helpers drop `"role":"assistant"` from the first delta;
   - the `usage_chunk` helpers drop `completion_tokens_details`;
   - `openai_responses` sets `sequence_number: 0` on each frame;
   - `gemini::tool_call_chunk` drops `index`, `role`, `modelVersion` and
     `responseId`, and trims `usageMetadata`.
   The id and model constants are already disclosed. Do not repeat them.
5. **Key order.** Each helper serializes a `serde_json::Value` from `json!`.
   `serde_json` 1.0.154 in `Cargo.lock` has no `preserve_order`, so the keys are
   in alphabetical order, unlike each cited fixture. Add one sentence to the
   crate-level doc of `conformance.rs`. It says this and that no translator
   depends on key order.
6. **`bedrock`.** Add the label HAND-AUTHORED (from the vendored
   `aws-sdk-bedrockruntime` types) to the module doc. State that the numeric
   values (11/4/15, `latencyMs` 314) are arbitrary. State that the order
   "metadata after messageStop" and "a real stream closes every block" have no
   capture.
7. **`gemini::done`.** Add the words "synthetic, not captured".
8. **`anthropic` (phase B).** Write this against the SMA-840 version of the
   module. Label `thinking_text_tool_use.txt` CAPTURED (transcribed, SMA-840)
   and the SMA-317 sources HAND-AUTHORED. Audit the helpers that SMA-840 adds
   (`thinking_start`, `thinking_delta`, `signature_delta`,
   `tool_use_start_at`).

### D10. Provider integration-test wire helpers (scope option 2)

Each helper and inline literal below that serves response bytes over HTTP gets
a comment with its label and source. Use `///` on a function. Use `//` on a
literal inside a function: `///` on a `let` statement triggers
`unused_doc_comments`, which `clippy -D warnings` makes an error. Most items are
HAND-AUTHORED. Name the fixture or capture whose shape each one copies, or write
"not recorded".

| File | Items | Phase |
|---|---|---|
| `crates/paigasus-helikon-providers-anthropic/tests/messages_wire.rs` | `empty_stream_response()`; the error bodies | B |
| `crates/paigasus-helikon-providers-anthropic/tests/prompt_caching.rs` | `turn()` | B |
| `crates/paigasus-helikon-providers-anthropic/tests/structured_output.rs` | `synth_tool_use_stream()` | B |
| `crates/paigasus-helikon-providers-gemini/tests/gemini_wire.rs` | `sse_ok()`; the 429 body (about line 86) | A |
| `crates/paigasus-helikon-providers-gemini/tests/gemini_streaming.rs` | the inline `data:` bodies (about 5) | A |
| `crates/paigasus-helikon-providers-litellm/tests/litellm_wire.rs` | `sse_ok()`; inline bodies (about 4) | A |
| `crates/paigasus-helikon-providers-litellm/tests/streaming.rs` | the module doc (lines 6-7) and the comment at lines 70-72, which say all fixtures are captured (apply D11); the inline body near line 54 | A |
| `crates/paigasus-helikon-providers-litellm/tests/cancellation.rs` | the `[DONE]` body | A |
| `crates/paigasus-helikon-providers-openai/tests/chat_wire.rs` | the SSE body (lines 30-44): relabel HAND-AUTHORED (id "x", `created` 1, model "gpt-4o", and a `"finish_reason":null` that no capture sends), replacing "Captured from LiteLLM"; the 429 body | A |
| `crates/paigasus-helikon-providers-openai/tests/chat_streaming.rs` | the inline body near line 225 | A |
| `crates/paigasus-helikon-providers-openai/tests/cancellation.rs` | the `[DONE]` body; the inline body near line 98 | A |
| `crates/paigasus-helikon-providers-openai/tests/responses_wire.rs` | `text_delta_event`, `completed_event`, `sse` | A |
| `crates/paigasus-helikon/tests/openai_litellm_message_parity.rs` | the SSE body near line 147 | A |

The line numbers come from an inventory and are approximate. The builder finds
each item again before editing.

### D11. A record that contradicts the new label

Some records say the opposite of the label that the evidence supports:

- the SMA-412 design (line 406) calls the Brave and Tavily JSON "captured";
- the SMA-451 design (line 783) says the streaming fixtures are "transcribed
  from real captured traffic ... not hand-written";
- `litellm/tests/streaming.rs` (lines 6-7, 70-72) says all fixtures are
  captured, "not hand-invented shapes".

Rule: label by the evidence. The header cites the contrary line and gives the
evidence for the label (for example `example.com` URLs, an invalid JSON line, a
finish value that no mock can produce, no capture in the PR or design). Do not
edit old design documents; they are history. Do correct a contrary claim in live
test code (`streaming.rs`), because a reader reads it as current.

## Data changes

The scope allows a data change only for a line that the wire did not send. The
research found no such line in the in-scope files except these, which are
synthetic on purpose and get a header note instead of a change:

- `tool_use_then_continuation.txt`: the `# --- turn 2 ---` delimiter.
- `unparseable_frame.txt`: the bad JSON line.
- `body_cut_inside_message_stop.txt`: the cut final event.

`openai_chat::text_delta` sends a `"finish_reason":null` that no capture
sends. It is recorded, not removed (D9.3). If the builder finds another line
that the wire did not send, record it in the header. Do not remove it without a
check with Sven.

## Sven's answers (spec gate, 2026-11-22)

- **Q1: yes and yes.** Add the D2 sentence to rule 2 (phase A). Correct
  "Added: nothing" in the worked example `responses_tool_call.txt` in this PR
  (phase A): the part becomes "not recorded", with the evidence that exists.
- **Q2: fix in phase B.** The five SMA-840 fixtures
  (`empty_thinking_tool_use.txt`, `interleaved_thinking.txt`,
  `redacted_thinking_tool_use.txt`, `thinking_text_tool_use.txt`,
  `unsigned_thinking_tool_use.txt`) get headers that obey the rule, in phase B
  of this PR, from the SMA-840 record (its design, plan and PR).
- **Q3: yes.** Add a header check test in `tests/workspace-lints/tests/`. For
  each `crates/*/tests/fixtures/*.{txt,sse}` file, it asserts that the first
  line starts with `: Provenance:` and one of the three labels, and that each
  line before the first `event:` or `data:` line starts with `:` or is blank.
  It has an allowlist for exempt files. Phase A adds the test with the
  Anthropic fixtures in the allowlist. Phase B removes them from the allowlist,
  so the allowlist can only shrink.

## Verification

- After each phase: `cargo test` for each touched crate and for
  `tests/provider-stream-conformance`. At the end:
  `cargo test --workspace --all-features`.
- `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-features --all-targets -- -D warnings` and
  `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps`.
- `npx markdownlint-cli2` (for `CONTRIBUTING.md` and this spec).
- **Data unchanged.** For each fixture, compare the old and new contents with
  all leading `:` lines and the blank line after them removed. They must be
  byte-identical, except for a recorded data change.
- **Header syntax.** Each line before the first `event:` or `data:` line of
  each touched fixture starts with `:` or is blank (the Q3 test).
- **Cut file.** The last byte of `body_cut_inside_message_stop.txt` is not
  `0x0a`.
- **Phase B.** After the rebase onto SMA-840: the Anthropic crate tests and the
  conformance tests pass again, and the cited SMA-840 paths exist on `main`.
- **PR body.** A claim-to-evidence table (file:line) for each new header.

## Left to the builder

- The exact wording of each header, within D1-D11.
- The order of the commits, one commit per crate or per fixture group.
- What to write when a cited spec line does not say what this design says: use
  "not recorded" and the evidence that does exist. Do not stop the work.
