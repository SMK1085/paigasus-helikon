# SMA-827: Gemini thought signatures round trip through a provider-data part in core

Linear: [SMA-827](https://linear.app/smaschek/issue/SMA-827). Related: SMA-820 (merge of same-role
contents, tool-result boundary), SMA-565 (stale `gemini-2.5-flash` ids).

## Intent

Gemini 3 models put a `thoughtSignature` on the `functionCall` parts of a response. The next
request must send each signature back on the same part. If a call in the current turn has no
signature, the API returns HTTP 400 "Function call is missing a thought_signature in functionCall
parts". The SDK reads no signature and sends none. So every tool loop on Gemini 3 fails at the
second model call. For a new user, every available Gemini model is a Gemini 3 model (the 2.x ids
return HTTP 404), so the Gemini provider cannot run a tool loop at all.

Core has no place for provider data. Anthropic has the same gap: its stream drops
`signature_delta` (`providers-anthropic/src/stream.rs:122`) and its request translator drops
`ContentPart::Reasoning` ("signature round-trip not yet supported"). Sven decided on 2026-11-20
that the new core shape must also hold Anthropic thinking signatures. This PR fixes Gemini only.

## Goals

1. Core gets one general carrier for opaque provider data, in the stream and in the history. The
   carrier can hold a Gemini signature for a tool call now, and an Anthropic `thinking` or
   `redacted_thinking` block later.
2. The Gemini stream translator keeps the `thoughtSignature` and the stream position of each
   `functionCall` part.
3. The Gemini request translator sends each signature back on the same part, with the calls in the
   order that Gemini sent them.
4. A history with no valid signatures (a stored session from before this change, a history from
   another provider or another Gemini model, a hand-built history) still works on Gemini 3.
5. The signature survives the session store and the durable runners, because it lives in the item
   history. (A handoff drops all tool calls from the transcript, see `thread_handoff_transcript`,
   so a signature has no call to attach to after a handoff. That is correct, not a loss.)

## Non-goals

- The Anthropic round trip. A follow-up Linear ticket does it with the same carrier.
- OpenAI Responses `encrypted_content` reasoning items. The carrier can hold them later.
- **Text-level signatures.** Gemini can also put a signature on a text part. Google documents it as
  optional to return. Core joins all `TokenDelta`s into one `Text` part, so the SDK cannot send it
  back on the part where it came. This PR ignores it. So only tool turns, which fail today, get the
  new part, and a text-only turn keeps today's stored format.
- Gemini thought summaries (`includeThoughts`, `thought: true` parts, translator item D3).
- Gemini 3 through other paths: the OpenAI provider on Gemini's OpenAI-compatible endpoint, and
  LiteLLM in front of Gemini 3. See "Left for Sven".
- Scripted evals (`evals/src/script.rs` `ScriptEvent`) cannot express the new event. No change.
- A new `AgentEvent` variant. Provider data stays inside `MessageOutput` items.
- An opt-out builder flag. Only tool turns change format, and those fail today without this change.
- SMA-565 (the stale model ids in the crate docs).

## Anti-goals

- A breaking change in core. Sven chose the additive approach (approach A).
- Provider data that reaches a translator it does not belong to, or that makes another provider's
  request invalid.
- A change to any behavior that users can see for a turn **without** provider data. This includes
  turns of other providers that today give `AssistantMessage[Reasoning]` with no `Text`.
- A change to the text output (final output, CLI, HTTP DTO `output`, workflow state, AG-UI frames)
  because a tool-only turn now has an `AssistantMessage`.

## Approaches

- **A. New additive variants (chosen by Sven on 2026-11-20).** Add `ModelEvent::ProviderData` and
  `ContentPart::ProviderData`. Both enums are `#[non_exhaustive]`, so this is not a breaking change,
  and core gets a patch bump. The data sits next to the call in the same `AssistantMessage`, not on
  the call. A tool-only turn now has an `AssistantMessage`, so the text consumers need a guard.
- **B. A new field on existing variants.** `provider_data` on `Item::ToolCall`,
  `ContentPart::ToolUse`, `ContentPart::Reasoning` and `ModelEvent::ToolCallDelta`. The data model
  is cleaner, but the variant fields are not `#[non_exhaustive]`: about 300 edits (130
  `Item::ToolCall` sites, 162 `ToolCallDelta` sites), broken user code, and a minor bump for core
  and every dependent. Rejected.
- **C. A signature cache in `GeminiModel` plus the dummy signature.** No core change, but the
  signatures do not survive a restart, a session store or a durable runner, and it does nothing
  for Anthropic. Rejected.

## Design

### Core types

```rust
// crates/paigasus-helikon-core/src/model.rs
ModelEvent::ProviderData {
    /// Wire-format key of the translator that owns `data`, for example "gemini".
    provider: String,
    /// The tool call that `data` belongs to, when it belongs to one.
    call_id: Option<String>,
    /// Opaque payload. Only the owning translator reads it.
    data: serde_json::Value,
}

// crates/paigasus-helikon-core/src/item.rs
ContentPart::ProviderData {
    provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    call_id: Option<String>,
    data: serde_json::Value,
}
```

Serde form: `{"type":"provider_data","provider":"gemini","call_id":"call_1","data":{...}}`.

The rustdoc of both variants states this contract:

- `provider` is a **wire-format key** that one translator owns. It is not `Model::provider()`.
  (OpenAI Chat and OpenAI Responses both report `"openai"`, but they will need different keys.)
  Each provider crate that writes provider data exports its key as a `pub const`, so that a host can
  filter or remove the data.
- A translator reads only the parts with its own key. It ignores every other part **without a
  warning**, and never fails because of one.
- The part is valid only inside an `Item::AssistantMessage`. Every translator ignores it in any
  other item.
- `call_id: Some(id)` means: the data belongs to the tool call with that id in the same model
  response (the `Item::ToolCall` items that follow the message, or a `ContentPart::ToolUse` in the
  same message). Call ids can repeat across responses.
- The position of a `ProviderData` part in `content` has **no meaning**. `ProviderData` parts keep
  their arrival order among themselves. A provider that needs block structure stores the whole
  block in `data`. (For the Anthropic follow-up: `{"type":"thinking","thinking":…,"signature":…}` or
  `{"type":"redacted_thinking","data":…}`, and its translator puts the blocks where its wire format
  needs them. The cost is that the thinking text is stored two times.)
- **Rollback note:** a reader on an earlier core version fails on `provider_data` with "unknown
  variant". After the first Gemini 3 tool turn, a session store or a Temporal history holds this
  variant. Deploy readers and Temporal workflow workers before writers, and do not roll back core
  below this version on a deployment that has stored such a turn. (The same hazard is documented for
  `SessionEvent::ToolDeferred`, `session.rs:152-154`.)

### Core helper and the text guard

A new public helper on `Item`:

```rust
/// True for an `AssistantMessage` whose content is not empty and holds only
/// `ContentPart::ProviderData` parts.
pub fn is_provider_data_only(&self) -> bool
```

Such a message did not exist before this change. So every text consumer that skips it keeps
today's behavior exactly, for every provider. The guard uses this predicate, and **not** "has no
`Text` part" (that predicate would change the behavior of `AssistantMessage[Reasoning]` turns from
OpenAI Responses, Anthropic, Bedrock and LiteLLM).

The session recorder needs the `MessageOutput` event (`session.rs:894`), so the event stays. These
consumers skip a provider-data-only message, so that they act as if the event did not exist:

- `runner.rs` `collect` and `collect_typed` (around lines 434 and 513).
- `workflow.rs` `assistant_text` and its callers in `workflow.rs` and `graph.rs`.
- `cli/src/repl.rs` (around line 221).
- `runtime-agentcore/src/agui/map.rs` (around lines 366-392).
- `runtime-axum` and `runtime-actix` `RunResponse::from_events` (they already skip an empty text;
  the guard makes the rule explicit).

The builder searches the workspace for every other consumer of `MessageOutput` or of an
`AssistantMessage` in the item history that takes text, and applies the same guard. The
structured-output path (`loop_state.rs` `last_assistant_content`) runs only on a turn with no tool
calls, so a provider-data-only message cannot reach it. The builder confirms that with a test.

### Core accumulator (`ModelTurnAccumulator`, `build_items`)

- `observe` keeps each `ModelEvent::ProviderData` in arrival order, in a new field.
- `build_items` makes the `AssistantMessage` when text, reasoning **or** provider data is not empty.
  Content order: `Reasoning`, `Text`, then the `ProviderData` parts in arrival order.
- Tool calls stay `Item::ToolCall` items after the message, in the existing `call_id`-sorted order.
- The rustdoc of `ModelTurn::items` changes to say the message can hold provider data only.

### Core: the other places that match on the new variants

- `token_counter.rs` `part_chars`: `ProviderData` counts the serialized length of `data`. The probe
  showed that Gemini bills the signature as input (see "Probe results"). Sven decided this after
  capture 6. Core does not know which provider will send the data, so it counts all of it.
- `agent.rs` event forwarding (around line 1475): `ModelEvent::ProviderData` is not forwarded as an
  `AgentEvent`. The accumulator still observes it.
- `agent.rs` injection allow-list (around line 1298) already rejects an unknown part. No change.
- The provider-stream conformance suite (`tests/provider-stream-conformance/src/check.rs`) gets a
  rule: a `ProviderData` with `call_id: Some(id)` names an id of a `ToolCallDelta` in the same
  stream.

### Other provider translators

Each translator already drops an unknown part with a wildcard arm. The changes:

- **OpenAI Chat and LiteLLM.** Today they emit `{"role":"assistant","content":""}` for an
  `AssistantMessage` with only unknown parts. When `Item::ToolCall` items follow, they merge into
  that message, and the result is valid. Alone, it is an empty assistant message. Change: an
  assistant message with no text and no tool calls is not emitted, and the following tool calls
  still form one assistant message. A `UserMessage` with only provider data must not emit
  `{"role":"user","content":[]}`.
- **Bedrock.** Add an explicit silent arm for `ProviderData`, so it does not warn on each request.
- **Anthropic, OpenAI Responses.** They already send nothing for an empty message. Add an explicit
  silent arm where a wildcard would warn.
- **Gemini `user_parts`.** Add an explicit silent arm.

### Gemini stream translator (`sse.rs`, `stream.rs`)

A capture in the repo (`tests/provider-stream-conformance/tests/conformance.rs:2225-2274`,
`gemini-3.6-flash`, 2026-08-20) shows: `functionCall.id` is present (`"call_1285087"`),
`thoughtSignature` is a sibling of `functionCall` in the same part (about 700 characters), and the
`STOP` chunk carries `{"text":""}` with no signature. The probe checks this again on 3.8.

- `sse.rs` `Part` gets `thought_signature: Option<String>` (`thoughtSignature` on the wire).
- For each `functionCall` part, the translator emits the `ToolCallDelta`, then
  `ProviderData { provider: "gemini", call_id: Some(<the same call_id>), data }`, where `data` is:

  ```json
  { "index": 0, "thoughtSignature": "<opaque>", "model": "gemini-3.8-flash", "api": "developer" }
  ```

  - `index` is the position of the call among the `functionCall` parts of this response (0, 1, 2,
    …), counted across chunks. It is present on **every** call, signed or not.
  - `thoughtSignature` is present only when the part has one.
  - `model` is the configured model id. `api` is `"developer"` or `"vertex"`. Both are present only
    with a signature.
- The `call_id` is the native id when present. Otherwise it is the existing synthesized
  `fc_<n>`. The request translator sends the same `call_id` as `functionCall.id`, so the pair matches
  in both directions.
- A signature on a part with no `functionCall` is ignored (see the text-level non-goal).
- The key `"gemini"` is exported as a `pub const` of the Gemini crate. The Developer API and Vertex
  share the translator, so they share the key, and `api` tells them apart.

### Gemini request translator (`translate/request.rs`)

**Pairing.** For each `AssistantMessage`, the translator first reads all its Gemini `ProviderData`
parts into a pending list of `(call_id, data)`. Then each `functionCall` (from a `ToolUse` part in
the same message, or from the `Item::ToolCall` items that follow) takes the first pending entry with
the same `call_id` and removes it. The pending list is cleared at the next `AssistantMessage` and at
the next `UserMessage` that is not only tool results. A `ToolResult` does not clear it, because
`close_open_transfers` can put a result between two calls of one response
(`resume_exec.rs:185,194`). The `ProviderData` part itself never becomes a wire part.

Trade-off for damaged histories: if items of two responses with the same call ids interleave with
no `AssistantMessage` between them, a signature can attach to the wrong call. Core never writes that
shape. The result is at worst an HTTP 400 for a history that is already damaged.

**Valid signature.** A signature is valid for this request when `data.model` equals the configured
model id and `data.api` equals the configured transport. A signature that is missing, malformed (not
a string) or not valid is treated as absent. A malformed part never causes an error.

**Order.** In each merged `model` content, when every `functionCall` part has an `index`, the
translator sorts the `functionCall` parts by `index` and keeps the other parts where they are. This
restores the order that Gemini sent. Otherwise the order stays as in the history.

**Signature on the part.** A valid signature goes on the same part:
`{"functionCall": {...}, "thoughtSignature": "..."}`.

**Fallback (dummy signature).** The current turn is every content after the last `user` content
that holds no `functionResponse` part. (SMA-820 shapes 2 and 4: Gemini checks signatures only in that
turn. The tool-result boundary keeps a text in its own content, so a mixed content is not built.) In
the current turn, a `model` content whose `functionCall` parts have no valid signature gets the
documented dummy `skip_thought_signature_validator` on each `functionCall` part. The SMA-820 probe
proved this shape (HTTP 200 on `gemini-3.8-flash`). Outside the current turn, no dummy is added.
This is a hard rule: a content with at least one valid signature is sent as received, and the
other calls stay unsigned, as Gemini returned them. Google says the dummy can make the answer worse,
so it is only the fallback. The dummy goes into one `const` with a doc comment and its source.
The dummy is sent only for Gemini 3 and later ids (the part of the model id after the last `/` must not
start with `gemini-1` or `gemini-2`). Gemini 1.x and 2.x requests keep today's shape and get no dummy;
real signatures are still sent back on every model.

**SMA-820 boundary.** The tool-result boundary in `merge_same_role` stays. Its doc comment changes:
the reason is no longer "the SDK sends no signatures". See "Left for Sven".

### Live check (before the Gemini code)

The Google pages that can be reached show no `generateContent` JSON for signatures, so the wire
format comes from live captures. The probe (bash, `curl`, `jq`) runs against the Developer API on
`gemini-3.8-flash`. It reads `GEMINI_API_KEY` from the environment and never prints a header or the
key. Sven runs it, because the agent shell has no key. Captures:

1. A streamed first turn that asks for three parallel calls. Record: where `thoughtSignature` sits,
   which parts carry it, whether `functionCall.id` is present, and whether the signature arrives in
   the same chunk as its call.
2. A streamed text answer: record whether a part carries a signature (information only).
3. Second turn, real signatures as received: expect HTTP 200.
4. Second turn, the signed call moved to the last position: record the result.
5. Second turn, the signed call first and the two unsigned calls swapped: record the result.
6. Second turn, the dummy on each call: expect HTTP 200. Compare `promptTokenCount` with capture 3.
7. Second turn, no signature: expect HTTP 400 (control).
8. SMA-820 shape 3 (`user[functionResponse…, text]` merged) with the real signature: record the
   result, for the boundary decision.
9. When `GEMINI_MODEL_ID_2` is set: capture 3 sent to the second model. Record whether Gemini
   accepts a signature from another model.

The core work and the work in other crates do not depend on the wire shape, so they can start
before the probe results. The Gemini stream and request work waits for the results. If capture 1
shows a different wire shape than this design expects, stop and bring it to Sven. Fixtures are
transcribed from the captures, with each real signature replaced by a placeholder. No real
signature goes into the repo or the PR body. Vertex is not probed; the spec states this limit in the
Gemini README.

### Probe results (2026-11-20)

Sven ran the probe against the Developer API on `gemini-3.8-flash`. No real signature value is
recorded here. Lengths are given.

**Capture 1 (streamed, three parallel calls).** HTTP 200, `finishReason` STOP. The stream has four
SSE chunks. Chunks 0 to 2 each hold one part with one `functionCall`. Chunk 3 holds `{"text": ""}`
with `finishReason: STOP` and no signature. `functionCall.id` is present on every call (native ids
of the form `call_<digits>`). `thoughtSignature` is a sibling of `functionCall` in the same part and
the same chunk. It is on the first call only, and it is 1140 characters long. The other two calls
have no signature. This matches the design.

Core sorts calls by `call_id` (`BTreeMap`). In this capture the sort moved the signed call from
first to last. So the order risk is real, and the `index` field is needed to restore the order.

**Capture 2 (streamed text answer).** Four text chunks. The last chunk is
`{"text": "", "thoughtSignature": <3036 characters>}`. A text-level signature exists, on an empty
final text part. The design ignores it (non-goal).

**Second turns (`generateContent`).**

| Capture | Shape | HTTP | finish | promptTokenCount |
|---|---|---|---|---|
| 3 | real signature as received | 200 | STOP | 351 |
| 4 | signed call moved to last | 200 | STOP | 351 |
| 5 | signed first, two unsigned calls swapped | 200 | STOP | 351 |
| 6 | dummy on each call | 200 | STOP | 183 |
| 7 | no signature | 400 "Function call is missing a thought_signature in functionCall parts", position 2 | - | - |
| 8 | SMA-820 shape 3, merged `user[functionResponse x3, text]`, real signature | 200 | STOP | 357 |
| 9 | other model | skipped (`GEMINI_MODEL_ID_2` not set) | - | - |

**Conclusions.**

- The wire shape matches the design. No stop condition triggers on capture 1 or 3.
- Gemini does not check the order of the calls (captures 4 and 5). The `index` sort is not required.
  The design keeps it, because it is harmless and it restores what Gemini sent.
- The signature is billed input: 351 against 183 prompt tokens for one 1140-character signature,
  about 168 tokens. This triggered the plan's stop condition. Sven decided that the core token
  estimate counts `ProviderData` as the serialized length of `data`, not 0.
- The merged shape (capture 8) gets HTTP 200 with the real signature. So the SMA-820 tool-result
  boundary is not needed when signatures are present. See "Left for Sven".
- Cross-model use and Vertex were not probed. They stay documented limits.

## Tests

- **Core accumulator:** a tool-only turn with provider data gives
  `[AssistantMessage[ProviderData…], ToolCall…]`; text plus provider data gives the order
  `Reasoning, Text, ProviderData`; no provider data gives the same items as today.
- **Core serde:** `tests/serde_roundtrip.rs` gets `content_part_provider_data_roundtrip` (with and
  without `call_id`) and a new snapshot. A `ModelTurn` with provider data survives a serde round
  trip (the Temporal `ModelTurnResult` path). Existing snapshots do not change.
- **Core helper:** `Item::is_provider_data_only` for empty content, PD-only, PD plus text, and
  reasoning-only messages.
- **Differential test (the anti-goal):** one scripted event stream, sent with and without
  provider-data events, through `collect`, `collect_typed`, the workflow agents, the AG-UI
  `EventMapper`, `RunResponse::from_events` and the REPL output. The outputs must be identical.
  Include a run that stops with `RunDeferred` after a tool-only turn.
- **Session:** `SessionRecorder` records `AssistantMessage[ProviderData]` before `ToolCalled`, and
  `project()` of that log, sent through the Gemini translator, puts the signature on the call.
- **Gemini stream:** a signed `functionCall` gives a `ToolCallDelta` and then `ProviderData` with the
  same `call_id`, `index`, `model` and `api`; an unsigned call gives `ProviderData` with `index`
  only; a text part with a signature gives no `ProviderData`. Fixtures come from the capture.
- **Gemini request (unit tests and insta snapshots):**
  - the round trip from stream events through `ModelTurnAccumulator` to request JSON;
  - two sequential rounds with the same `call_id` keep each signature in its own round;
  - three parallel calls whose `call_id` sort order differs from `index` are sent in `index` order,
    with the signature on the first call;
  - a history with no signatures gets the dummy on each call in the current turn only;
  - a signature with another `model` or `api` is replaced by the dummy;
  - a malformed signature is treated as absent;
  - a foreign `ProviderData` part is ignored with no warning;
  - a provider-data-only message with no following calls emits no `model` content.
  The existing exact-JSON tests and snapshots that change because of the dummy
  (`snap_tool_call_and_result`, `snap_parallel_same_name_tool_calls`, and the tests in
  `translate/request.rs` with a call in the current turn) are updated, and each change is checked.
- **OpenAI Chat and LiteLLM:** a provider-data-only assistant message alone emits nothing; followed by
  two tool calls it gives one assistant message with two tool calls; a provider-data-only user
  message emits nothing.
- **Bedrock, Anthropic, OpenAI Responses:** a provider-data part is dropped with no warning.
- **Live (`#[ignore]`, `gemini-3.8-flash`):**
  1. A two-turn tool loop through `GeminiModel::invoke`, with no user text after the tool result, so
     the call is in the current turn. It must end with `Finish` and no error. It also asserts that
     turn 1 gives a `ProviderData { provider: "gemini", call_id: Some(_) }` with a non-empty
     signature, and that the turn-2 request body holds that signature and not the dummy. (The builder
     puts this test where it can read the request body: in `src/` with `#[cfg(test)] #[ignore]`, or
     with a transport that records the request.)
  2. An `LlmAgent` loop with two sequential tool rounds and a real tool.
  3. An `LlmAgent` with an `output_type` and one tool (the structured-output finalizing request).

## Docs and release

- The core rustdoc on both variants and on `Item::is_provider_data_only` states the contract and
  the rollback note.
- mdBook: `docs/book/src/concepts/core-primitives.md` (around line 35, the new variant),
  `model-providers.md` (around line 55, and the Gemini limitations section around lines 296-300),
  `sessions.md` (around lines 270-274, provider data counts its data length in the token estimate), and
  `agent-loop.md` (around lines 64-69: `MessageOutput` now also comes on a Gemini tool-only turn).
- `crates/paigasus-helikon-providers-gemini/README.md` limitations section (around line 89): tool
  loops on Gemini 3 work; Vertex signatures are not live-tested; text-level signatures are not sent.
- CHANGELOG (written by release-plz from the commits): the commit body names the behavior change —
  code that renders each `MessageOutput` now also gets a provider-data-only message on every Gemini 3
  tool turn — and the rollback note.
- **Release: no hand bump.** The Gemini crate and every other touched crate are already released, so
  release-plz bumps core, the touched crates and the facade (cascade) in one release PR and publishes
  them in dependency order, core first. A hand bump of core would stop the facade cascade
  (`docs/runbooks/release-ascend.md`, caveat 2; proven pure-auto in SMA-418, PR #80). The same-PR
  core bump of caveat 1 applies only to a crate whose version is bumped by hand in the PR.
  cargo-semver-checks must stay green: new variants on `#[non_exhaustive]` enums and a new method
  are not breaking.

## Left to the builder

- The exact names of private helpers and of the pending-signature struct.
- The name of the exported key constant.
- How to keep the dummy-signature tests readable (a helper that builds a signed history).

## Left for Sven

- **The SMA-820 tool-result boundary.** Capture 8 gave HTTP 200 for the merged shape
  `user[functionResponse x3, text]` with the real signature. So the boundary is not needed when
  signatures are present. My proposal: keep the boundary in this PR, because it changes nothing
  that works today. Sven decides on its removal.
- **Follow-up tickets.** After approval I file in Linear: (1) the Anthropic thinking round trip with
  this carrier; (2) the Gemini 3 signature error through the OpenAI provider on Gemini's
  OpenAI-compatible endpoint and through LiteLLM, if Sven wants it tracked.
