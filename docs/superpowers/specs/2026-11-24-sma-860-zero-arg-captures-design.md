# SMA-860: capture a zero-argument tool-call stream for five conformance subjects

- **Linear:** SMA-860 (follow-up of SMA-567)
- **Date:** 2026-11-24
- **Branch:** `feature/sma-860-capture-a-zero-argument-tool-call-stream-for`

## 1. Intent

SMA-567 added the `ZeroArgToolCallCleanStop` scenario to the stream conformance suite
(`tests/provider-stream-conformance`). Only `openai/responses` serves it. Five subjects are pinned
in the `UNCAPTURED` table with SMA-860 as the follow-up: `anthropic`, `bedrock`, `gemini`,
`litellm` and `openai/chat`.

The suite accepts a fixture only if it meets the provenance rule (`CONTRIBUTING.md` → "Wire-format
fixtures"). So each of the five subjects needs a real capture before it can serve the scenario.
This change commits the five captures, serves the scenario from them, and removes the five rows.

## 2. Goals, non-goals, anti-goals

**Goals.**

- G1. Each of the five subjects serves `ZeroArgToolCallCleanStop` from a committed capture.
- G2. Each committed capture is byte-identical to the wire, and its provenance is recorded.
- G3. A test fails when a script's copied payloads differ from the committed capture.
- G4. The five `UNCAPTURED` rows for `ZeroArgToolCallCleanStop` are removed.
- G5. The design records what each provider actually sends for a zero-argument call. The ticket
  asks for this, in particular for a stream with no argument deltas and for the Gemini `null`
  arguments.

**Non-goals.**

- No change to a provider translator, unless the suite finds a conformance failure (SMA-533
  design §9 applies). §5 predicts that no failure occurs.
- No change to the `ReasoningCleanStop` rows (SMA-858) or the `ParallelToolCallCleanStop` rows
  (SMA-859).
- No new scenario and no translator fix for a stream with truly zero argument deltas. No capture
  shows one (§4). §6.7 files a follow-up instead.

**Anti-goals.**

- A capture that is labelled CAPTURED but has an edit that nothing records.
- A script that "fixes" a capture so that it matches a sibling subject.

## 3. Approach

Three approaches were considered:

- **A. Commit a fixture file and copy its payloads into `conformance.rs`.** This is the
  `ZERO_ARG_EVENTS` pattern of the `openai/responses` subject (SMA-567). The SMA-567 design (§6.9)
  requires it. Its risk is drift between the copy and the file.
- **B. Commit a fixture file and `include_str!` it from the script.** No drift can occur. But this
  adds a new serving pattern and a frame parser for each wire format.
- **C. Record the capture only in the module doc** (the Gemini "Live capture" pattern). This is the
  weakest provenance, because doc comments usually elide values.

**Sven picked A** on 2026-11-24. The spec challenge showed that review alone does not stop drift:
the anthropic, gemini and litellm drivers skip an unparseable `data:` payload with only a
`tracing::warn!`, so a copy error can drop a frame and still pass every floor. So this design keeps
A for serving and adds one guard test for each subject that compares the consts with the committed
file (§6.3). The guard reads the file, but the script does not.

## 4. The captures

Claude ran the captures on 2026-11-24 with Sven's keys and the AWS profile `maip-dev-admin`, at
Sven's request. Each request offered one tool, `get_current_time`, with the description "Return
the current server time. Takes no arguments." and the user message "What time is it?". Each
request forced the tool call. Request headers (keys) are not recorded.

| Subject | Endpoint | Model | Time (UTC) | Argument deltas on the wire |
|---|---|---|---|---|
| `anthropic` | `https://api.anthropic.com/v1/messages`, `anthropic-version: 2023-06-01` | `claude-haiku-5-5` | 20:38:59 | One `input_json_delta` with `partial_json: ""`. `content_block_start` carries `"input":{}`. |
| `openai/chat` | `https://api.openai.com/v1/chat/completions` | `gpt-5-mini` (wire: `gpt-5-mini-2025-08-07`) | 20:39:21 | The first `tool_calls` delta has `arguments: ""`. One more delta has `arguments: "{}"`. |
| `gemini` | `https://generativelanguage.googleapis.com/v1beta/models/gemini-3.8-flash:streamGenerateContent?alt=sse` | `gemini-3.8-flash` | 21:14:44 | No deltas. One `functionCall` part with `"args": {}`. |
| `bedrock` | `https://bedrock-runtime.eu-central-1.amazonaws.com/model/eu.anthropic.claude-haiku-5-5/converse-stream`, sent with `curl --aws-sigv4` | inference profile `eu.anthropic.claude-haiku-5-5` | 20:39:45 | One `contentBlockDelta` with `toolUse.input: ""`. |
| `litellm` | LiteLLM 1.98.0 proxy (`ghcr.io/berriai/litellm@sha256:3a1980074ff2ac13883e0ffee1bebec345d8addd004be251079cee173b0e8305`, the SMA-547 digest), `/v1/chat/completions` | proxy alias `zero-arg-anthropic` (the wire `model` value), upstream `anthropic/claude-haiku-5-5` | 20:59:06 | Three `tool_calls` deltas: `""`, `""`, `"{}"`. |

The committed files (§6.1) are the bytes as received. Their sizes and SHA-256:

| Subject | Bytes | SHA-256 |
|---|---|---|
| `anthropic` | 1364 | `b6114d956d7d80a6b315fcd2f6866fff8c776e9ebbed846c8ab7ccf9093dd86e` |
| `openai/chat` | 1618 | `5c5e74ac372aa0f954e89801c0addb62592a1ae02ad3c36b427b4d24f56d8cf2` |
| `gemini` | 1383 | `45271e2e2c96d08f183bec5251811498fee18b74c3cbfc92f9a2fc7c35542624` |
| `bedrock` (binary) | 1250 | `9196618e83783e261b5a3997d81a8cb343ff58bb1c560bff611a53c0d6a98b04` |
| `litellm` | 1739 | `b81bc790cdcf6a93e1ef6e8391a353a344d95549ef08a412ee0f7c1f241552c7` |

Check with `shasum -a 256 tests/provider-stream-conformance/fixtures/*/zero_arg_tool_call.raw`.

**Findings.**

1. **No provider sent a stream with truly zero argument deltas.** Anthropic and Bedrock send one
   delta with an empty string. So the accumulated arguments text is `""`, not `{}`. Gemini sends
   the whole call in one part. A zero-delta stream still has no capture for any subject.
2. **The `anthropic` and `bedrock` translators depend on that empty delta.** Both emit the tool
   name only with the first input delta (anthropic `src/stream.rs` near 173-193, bedrock
   `src/stream.rs` near 196-219). On a stream with no input delta they emit no `ToolCallDelta` but
   still emit `Finish(ToolCalls)`. `openai/chat` and `litellm` have `flush_buffered_names` for this
   shape, and `openai/responses` has the SMA-562 synthesis. §6.7 files a follow-up.
3. **Gemini sent `"args": {}`, not a missing `args`.** The `"null"` path in
   `providers-gemini/src/sse.rs` did not occur. An earlier Gemini capture at 20:39:23 declared the
   tool with no `parameters` field. The SDK never sends that (`translate/tools.rs` always sends
   `parameters`), so it was captured again at 21:14:44 with `{"type":"object","properties":{}}`.
   Both captures show the same shape. Only the second one is committed.
4. **Gemini ends each SSE line with CRLF.** The other four text captures use LF. The existing
   `.gitattributes` rule `crates/paigasus-helikon-providers-gemini/tests/fixtures/*.sse text
   eol=lf` would change these bytes. So the committed file has the `binary` attribute (§6.1).
   `parallel_signed.sse` (SMA-827) is LF in git. The record does not show whether its wire was
   CRLF.
5. **LiteLLM emits `""`, `""`, `"{}"` for an Anthropic upstream.** The direct Anthropic capture
   (20:38:59) sent one `""`. The LiteLLM run made a different upstream call, which nobody captured.
   So "LiteLLM adds the `{}`" is an inference, not an observation.
6. **Bedrock sends `messageStop` before `metadata`.** This is the first recorded Bedrock order:
   `messageStart`, `contentBlockStart`, `contentBlockDelta`, `contentBlockStop`, `messageStop`,
   `metadata`. The `toolUse` start carries `"type":"tool_use"`, and `metadata.usage` carries
   `serverToolUsage:{}`.
7. **Each capture has fields that the translators do not use.** Examples: `caller` (Anthropic,
   LiteLLM), `p` padding (Bedrock), `obfuscation` (OpenAI), `inference_geo`, and spaces before the
   closing `}` on five Anthropic `data:` lines. The scripts keep them, because a CAPTURED payload
   is sent byte for byte.

## 5. Expected suite behaviour

The spec challenge traced each shape through the current translators:

- `anthropic` ignores `content_block_start.input` and `caller`. The empty `input_json_delta` emits
  the name with `""`. `ping` is a no-op.
- `bedrock`: the `input: ""` delta emits the name. The SDK skips `p` and `serverToolUsage`.
- `gemini`: `args {}` becomes `"{}"`. A bare EOF runs `finish()`. `STOP` after a `functionCall`
  gives `ToolCalls`. `eventsource-stream` accepts CRLF.
- `litellm`: chunk 1 buffers the name, chunk 2 flushes it with `""`, chunk 3 adds `{}`.
- `openai/chat`: async-openai 0.43 knows `obfuscation`.

The `ToolCallsMismatch` floor (`src/lib.rs` near line 1032) and assertion 10 (`src/check.rs` near
line 192) both read blank arguments as `{}`. So the `""` arguments of `anthropic` and `bedrock`
match the declared call `get_current_time` with `{}`. No translator change is expected. If a
subject fails, the builder stops, records the failure, and treats it as a SMA-533 §9 finding. The
builder does not edit the capture to make it pass.

## 6. Design

### 6.1 Fixture files

The captures live in the unpublished suite crate, not in the provider crates. So no provider crate
gets a release for test-only files, and no real request id goes into a crates.io package. The
files are already in the worktree (untracked):

```text
tests/provider-stream-conformance/fixtures/<subject>/zero_arg_tool_call.raw
tests/provider-stream-conformance/fixtures/<subject>/zero_arg_tool_call.request.json
tests/provider-stream-conformance/fixtures/litellm/zero_arg_tool_call.litellm_config.yaml
```

`<subject>` is `anthropic`, `bedrock`, `gemini`, `litellm` or `openai_chat`.

- Each `.raw` file is the response body exactly as received. It has no header, so that its hash
  matches §4 and no edit exists to record.
- Add `tests/provider-stream-conformance/fixtures/**/*.raw binary` to `.gitattributes`. Put it
  before the bedrock `binary` rule, which must stay last. This keeps the Gemini CRLF bytes and the
  Bedrock frames unchanged on every platform.
- CONTRIBUTING rule 5 allows provenance "in the doc comment of the test that loads it" for a file
  that has no comment syntax. A `.raw` file deliberately has no header. So its provenance goes in
  the doc comment of its guard test (§6.3): label CAPTURED, endpoint, model, date and time, the
  request file, the byte count and SHA-256, the §4 finding for that subject, and "Removed: nothing.
  Added: nothing. Changed: nothing."
- Update the `.gitattributes` comments that say "Presently inert" (lines 17-25 and 38-50): the
  fixtures directory now has files.

### 6.2 Scripts

In each of the five subject modules in `tests/provider-stream-conformance/tests/conformance.rs`:

- Replace the `Outcome::Uncaptured(..)` return for `ZeroArgToolCallCleanStop` with a served
  script.
- Copy the capture payloads into a const in the form of `openai_responses::ZERO_ARG_EVENTS`.
  Each payload is the exact text of a `data:` line (with its `event:` name where the wire has one).
  Anthropic payloads keep their spaces before the closing `}`.
- Send every frame of the capture, in order: the Anthropic `ping` event, and the `data: [DONE]`
  frame of `openai/chat` and `litellm`. Gemini ends at bare EOF, as captured. Do not add the
  synthetic `done()`.
- The existing `frame` helpers of `anthropic`, `openai_chat` and `litellm` take a `Value`, which
  drops the Anthropic padding and sorts the keys. Add a `captured_frame(&str ..)` helper to each
  of these modules, in the form of `openai_responses::captured_frame`.
- Gemini sends the CRLF framing of the capture (`data: …\r\n\r\n`), so that the bytes equal the
  wire.
- Bedrock: add `pub fn frame_raw(event_type: &str, payload: &[u8]) -> Vec<u8>` to
  `src/eventstream.rs`, and make `frame` call it. The script sends each captured payload through
  `frame_raw`, so no `serde_json` round trip occurs. The prelude, the header block and the CRCs
  are encoded again by the suite. Record that under "Edits made here", with the header order
  that `frame` writes. The payload bytes are unchanged.
- Write the arm comment in the form of the `openai_responses` arm: the source file, the capture,
  and "Edits made here", which must be framing only. The `anthropic` and `bedrock` comments must
  say that the wire sent `""` arguments and that the floor reads blank as `{}`.
- `request()` for this scenario offers `get_current_time` with the schema
  `{"type":"object","properties":{}}` and forces the call (the `ToolChoice` that names the tool),
  to match the captured requests.
- `fixture_calls()` returns `get_current_time` with `{}`. `fixture_finish_reason()` returns
  `Some(FinishReason::ToolCalls)`.
- Add the new frames to each subject's `scan_finds_only_*` test, as the gemini `PARALLEL_SIGNED`
  arm does.

### 6.3 Guard tests

One `#[test]` for each subject, in its module. Its doc comment is the provenance record of §6.1.

- SSE subjects: `include_bytes!` the `.raw` file, split it into frames, and assert that the
  `event:`/`data:` values equal the const, in order and complete. One shared helper does the split
  for all four SSE subjects. It must accept both LF and CRLF.
- Bedrock: `include_bytes!` the `.raw` file and decode each frame with `MessageFrameDecoder`, in
  the way that the existing `event_types` helper does. Assert that each `(:event-type, payload)`
  pair equals the const, in order and complete.
- Each guard must fail when one byte of a copied payload changes. The builder proves this once for
  each subject with a temporary edit, then reverts it.

### 6.4 Tables and mock subjects

- `src/declines.rs`: remove the five `ZeroArgToolCallCleanStop` rows from `UNCAPTURED`, and
  `Scenario::ZeroArgToolCallCleanStop` from `UNCAPTURED_SCENARIOS`. No row names it any more, and
  a future row must again need a reviewed edit. Update the comment above the removed rows.
- `src/lib.rs` mock subjects: `Subject::conforming` reports `ZERO_ARG_UNCAPTURED` for
  `openai/chat`, `anthropic` and `bedrock` (near lines 1347-1375, including `BEDROCK_UNCAPTURED`).
  These mocks run against the real `UNCAPTURED` table. So without a change,
  `a_conforming_subject_passes` fails, and `an_uncaptured_outcome_for_a_declined_pair_fails`
  passes for two reasons at once. Remove `ZERO_ARG_UNCAPTURED` from the three `conforming` arms,
  from `BEDROCK_UNCAPTURED`, and from the overrides near lines 1861 and 2243. For its test-only
  use near line 1829, keep the test's one purpose: if it needs an uncaptured pair, use a pair that
  is still in `UNCAPTURED` (a `ReasoningCleanStop` or `ParallelToolCallCleanStop` row). Update
  the doc near line 1326.
- Add a `src/lib.rs` unit test: a zero-argument call with `args_delta: ""` against the declared
  `{}` gives no `ToolCallsMismatch`. Today only assertion 10 has this test (`blank_args_conform`).

### 6.5 Doc comments that become false

All are under `tests/provider-stream-conformance/` unless named otherwise. Line numbers are
approximate.

- `tests/conformance.rs` 51-90, the `bedrock` module doc: "Label: HAND-AUTHORED … No helper here
  is a capture", "the repo holds no captured Bedrock traffic", the section "Why there are no `.bin`
  fixture files", and "No capture records the order".
- `tests/conformance.rs` 116, 171-172 and 188-192: "No capture records that a real Bedrock stream
  sends `metadata` after `messageStop`". The new capture shows this order (§4 finding 6). The
  hand-authored scripts for the other scenarios do not change.
- `tests/conformance.rs` 672-673, the `openai_chat` doc: "No capture of `api.openai.com` Chat
  Completions is recorded". Also 681-684 and 797-799: "No recorded capture sends that key". The
  new capture sends `"finish_reason":null`.
- `tests/conformance.rs` near 2952, the `gemini` provenance list, and near 2023-2026, the
  `anthropic` text "The three tool scripts".
- `src/lib.rs` 67-68: `ZeroArgToolCallCleanStop` is "(a `{}` delta)". Anthropic and Bedrock send
  `""`, and Gemini sends no delta.
- `src/eventstream.rs` 1-12, the module doc (it gains `frame_raw` and is now used for captured
  payloads).
- `.gitattributes`, the two "Presently inert" comments.

The builder also searches for `SMA-860` and "only `openai/responses`" and corrects each hit.

### 6.6 Release and book

- No provider crate changes, and the suite crate has `publish = false`. So release-plz releases
  nothing.
- No mdBook or README change. This changes an unpublished test crate and no public API.

### 6.7 Linear follow-up

Filed after Gate 1 as **SMA-863**: "anthropic and bedrock emit no ToolCallDelta for a tool call
with no input delta" (§4 finding 2). It is a translator gap with no capture of the wire shape. It
is linked to SMA-860 and SMA-562.

### 6.8 Merge order

SMA-858 and SMA-859 change the same table, the same mock arms and the same `Uncaptured` arms. The
PR that merges second rebases onto `main` and resolves the conflicts. No order is required.

### 6.9 Verification

- `cargo test -p <suite package name>` passes.
- For each subject, a temporary change of the copied arguments of the call to `{"x":1}` fails the
  suite with `ToolCallsMismatch`. Revert it.
- For each subject, a temporary one-byte change in a copied payload fails its guard test. Revert it.
- For `anthropic` and `bedrock`, a temporary removal of the empty input delta from the script.
  Record the result in the PR body (expected: the "no `ToolCallDelta`" floor fails). Revert it.
- The full CI gate list in `CLAUDE.md`.

## 7. Left to the builder

- Const names, helper names and the frame-split helper's location, if they follow the existing
  modules.
- The exact wording of each provenance doc comment, inside the rules in §6.1.
