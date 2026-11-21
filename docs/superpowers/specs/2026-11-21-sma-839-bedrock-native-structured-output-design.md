# SMA-839 — Bedrock: no forced `toolChoice` for Claude Sonnet 5.5 and Opus 5.5

- **Linear:** [SMA-839](https://linear.app/smaschek/issue/SMA-839/bedrock-sends-a-forced-toolchoice-to-claude-sonnet-55-and-opus-55)
- **Crate:** `paigasus-helikon-providers-bedrock`
- **Date:** 2026-11-21
- **Status:** approved by Sven on 2026-11-21 (section 8 question 1: the draft answer, omit with `warn!`, stands)

## 1. Intent

The Bedrock provider gates forced tool choice on the model family.
`ModelFamily::supports_forced_tool_choice` returns `true` for every `Anthropic`
model. Claude Sonnet 5.5 and Claude Opus 5.5 reject a forced `toolChoice`:

```text
ValidationException: The model returned the following errors:
tool_choice: type "tool" and "any" are not supported for this model.
```

Thus, on these two models:

- `ResponseFormat::JsonSchema` and `JsonObject` fail, because the provider
  synthesizes them as a forced tool call.
- `ToolChoice::Required` (`toolChoice: any`) and `ToolChoice::Tool`
  (`toolChoice: tool`) fail.

This change makes structured output work on these models through Bedrock
**native structured output** (`outputConfig.textFormat`). It also stops the
provider from sending a forced `toolChoice` to them.

## 2. Evidence (live probes, 2026-11-21, us-east-1)

The probes used the AWS CLI 2.39.6 (`Converse`) and boto3 through `uv`
(`ConverseStream`), with the `maip-dev-admin` profile. Unless a row says
otherwise, the prompt was `Give a transaction: deposit of 100 USD.` and
`maxTokens` was 256.

### 2.1 Tool choice and output paths

| Request | Model | Result |
| --- | --- | --- |
| `toolChoice: any` or `tool` | Sonnet 5.5, Opus 5.5 | `ValidationException` (model-layer text above) |
| `toolChoice: any` or `tool` on `ConverseStream` | Sonnet 5.5 | HTTP 400 from the initial call, same text. Not an in-stream event. |
| `toolChoice: any` | Haiku 5.5, Sonnet 5, Opus 5, Nova Pro | Accepted, `tool_use` |
| `toolChoice: any` | Llama 3.3 70B | `This model doesn't support the toolConfig.toolChoice.any field. Remove toolConfig.toolChoice.any and try again.` (Bedrock-layer text) |
| `toolChoice: auto` + one tool | Sonnet 5.5 | The model thinks and gives no JSON (`max_tokens`) |
| `outputConfig` JSON schema, `Converse` | Sonnet 5.5, Opus 5.5 | Valid JSON, `end_turn` (exact acceptance prompt and 256 tokens) |
| `outputConfig` JSON schema, `ConverseStream` | Sonnet 5.5 | HTTP 200, text deltas form valid JSON, `end_turn` |
| `outputConfig` + a user tool (auto) | Sonnet 5.5, Opus 5.5 | Valid JSON, `end_turn` |
| `outputConfig`, history with `toolUse`/`toolResult`, no `toolConfig` | Sonnet 5.5, Opus 5.5 | `The toolConfig field must be defined when using toolUse and toolResult content blocks.` |
| Same, plus the synthesized tool as `toolConfig`, no `toolChoice` | Sonnet 5.5, Opus 5.5 | Valid JSON as text, `end_turn` |
| Same, plus a stub tool from the history, no `toolChoice` | Sonnet 5.5, Opus 5.5 | Valid JSON as text, `end_turn` |
| `outputConfig` | Haiku 4.5, Haiku 5.5, Sonnet 4.5, 4.6, 5, Opus 4.5, 4.6, 4.7, 4.8, 5 | Valid JSON |
| `outputConfig` | Opus 4.1 | `output_config: Extra inputs are not permitted` |
| `outputConfig` | `global.amazon.nova-2-lite-v1:0` | `This model doesn't support the outputConfig field.` |

### 2.2 Native output schema support (Sonnet 5.5)

| Accepted | Rejected by Bedrock |
| --- | --- |
| `$ref` with `$defs` or `definitions`, `anyOf`, `allOf`, `enum`, `const`, `format` (also `uint32`, `double`, `date`), `pattern`, `minLength`, `maxLength`, `minItems` 0 or 1, `type: ["number","null"]`, `type: ["object","null"]`, `$schema`, `title`, `description`, `default`, `examples`, optional properties | an object without `additionalProperties`, `additionalProperties: true`, `minimum` / `maximum` / `exclusiveMinimum` / `exclusiveMaximum` (number and integer), `oneOf`, `minItems` > 1, `maxItems`, `uniqueItems`, `prefixItems`, `patternProperties`, `propertyNames`, `not`, `if`/`then`/`else`, the boolean schema `true`, the empty schema `{}`, a recursive `$ref` |

`multipleOf` was not probed. The schema `name` accepted spaces, `<>`, `_` and
100 characters.

## 3. Goals

1. A model that is known to reject a forced `toolChoice` never gets one.
2. `ResponseFormat::JsonSchema` on Sonnet 5.5 and Opus 5.5 returns JSON that
   agrees with the **rewritten** schema (section 7.5), through native output.
   This includes the core finalizing turn after a tool round.
3. A future Claude model that also rejects a forced `toolChoice` works without
   a code change. It costs one rejected request for each model handle.
4. `capabilities()` tells the truth for these models.
5. The crate README and the mdBook Bedrock section are correct.

## 4. Non-goals

- Native output for the other Claude models. They keep the forced-tool path,
  which works today.
- Native output for Nova or Mistral. Nova rejects `outputConfig`.
- A public API to select the path. The builder gets no new method, and
  `Ruleset` gets no new variant.
- Effort control (`outputConfig.effort`). The probes show that 256 tokens are
  enough for the acceptance test.
- The direct Anthropic provider. It also sends a forced `tool_choice`
  (`providers-anthropic/src/translate/mod.rs:142-144`). This is not verified on
  Sonnet 5.5 and Opus 5.5. Track it in a separate Linear issue.
- `ToolChoice::None` with tool history and no `JsonSchema`. It sends no
  `toolConfig` and fails today on every Claude model. This is older than this
  change. Track it in a separate Linear issue.

## 5. Anti-goals

- A forced `toolChoice` must not go to Sonnet 5.5 or Opus 5.5, also not on the
  first request.
- The fallback must not hide other errors. Only the two exact rejection texts
  start it. It occurs at most one time for each request.
- The fix must not change the request for models that work today. Haiku 5.5,
  Opus 5 and Sonnet 5 keep the forced-tool path, byte for byte.

## 6. Approaches

The output path is native output. It is the only path that can pass the
acceptance test: degrade-to-text gives prose, and `toolChoice: auto` gave no
JSON. The open question was how the provider selects the models that use it.

| | Approach | Trade-off |
| --- | --- | --- |
| A | A model rule: IDs that contain `claude-sonnet-5-5` or `claude-opus-5-5` use native output. | Simple and deterministic. Each new model with the same limit needs a code change. A missing entry fails with an error, not without one. |
| B | Learn at runtime: send the forced `toolChoice`; on the exact rejection, remember it on the model handle and send again once with native output. | No list. Each new model handle sends one rejected request first. The code matches Bedrock error text. |
| **C** | **A and B together.** | **Chosen by Sven.** Known models never get a forced `toolChoice` (A). Future models recover without a release (B). |

## 7. Design

### 7.1 The structured-output path

Add a crate-private enum:

```rust
pub(crate) enum OutputPath {
    /// Today's behavior: JsonSchema/JsonObject → forced synthesized tool.
    ForcedTool,
    /// Native structured output; no forced toolChoice of any kind.
    Native,
}
```

`build_request` gets the path as an input. Every guard and check that today
reads `family.supports_forced_tool_choice()` reads the path instead, where the
path is `Native`. On the `ForcedTool` path, the family checks stay as they are.

### 7.2 The forced-choice policy (rules A and B together)

Put both rules in one small crate-private type, for example:

```rust
pub(crate) struct ForcedChoicePolicy {
    rejects_static: bool,   // rule A, fixed at build()
    learned: AtomicBool,    // rule B, set at runtime
    family: ModelFamily,
}
impl ForcedChoicePolicy {
    fn path(&self) -> OutputPath;              // Native if rejects_static || learned
    fn record_rejection(&self) -> bool;        // swap(true); true if this was the first time
}
```

`BedrockModel` holds the policy next to `Config`, for example
`BedrockModel { config: Arc<Config>, policy: Arc<ForcedChoicePolicy> }`, or in
one shared inner struct. Do not put the `AtomicBool` into `Config`: `Config`
derives `Clone` and is fixed configuration. All clones of one `BedrockModel`
share the policy. A new `BedrockModel` starts with `learned = false`.

The policy is per handle. A process-wide set keyed by model ID would save the
rejected request for short-lived handles, but it adds global mutable state.
This is a builder-visible call; see section 10.

### 7.3 Rule A — the static model rule

Add `pub(crate) fn rejects_forced_tool_choice(model_id: &str) -> bool`. It
returns `true` when the family is `Anthropic` and the lowercased resource ID
contains `claude-sonnet-5-5` or `claude-opus-5-5`. It uses the same resource-ID
logic as `from_model_id`: strip an ARN to the text after the last `/`, and
accept any geographic prefix.

The match must not hit `claude-sonnet-5` or `claude-opus-5`. Those models accept
a forced `toolChoice` (section 2.1).

**Why a list is acceptable here.** The ticket asks for no list that goes stale
without an error. This list goes stale *with* an error: a new model that is not
on it gets a clear `ValidationException`, as on `main` today. Rule B then
recovers from that error at runtime. So a stale list costs one rejected request,
not a wrong answer.

### 7.4 Rule B — the runtime fallback

**Scope.** Rule B applies only to the `Anthropic` family. Native output is
known to work only there; Nova rejects `outputConfig`.

**Classifier.** Add a pure function next to `classify` in `error.rs`. It works
on the raw `svc.err().code()` and `svc.err().message()`, before
`map_sdk_http_error`. It returns `true` only when the code is
`ValidationException` and the message contains one of:

- `type "tool" and "any" are not supported` (the model layer, section 2.1),
- `doesn't support the toolConfig.toolChoice.` (the Bedrock layer, as Llama
  returns it).

**Flow.** Write the send-and-fallback logic as a generic async function over a
send closure, so that unit tests can use fakes. In `invoke`:

1. Get the path from the policy. Build the request with it.
2. Send it. The rejection arrives as an error from `send()` (HTTP 400). It does
   not arrive as an in-stream event (section 2.1).
3. If all of these are true, start the fallback:
   - the family is `Anthropic`,
   - the sent request had a forced `toolChoice` (`any` or `tool`),
   - the classifier returns `true`,
   - this request has not done a fallback already.
4. The fallback: call `record_rejection()`. If it returns `true`, log one
   `warn!` that names the model ID and says that native output is now in use
   for this handle. Rebuild the request with `OutputPath::Native` and send it
   once.
5. If the second send fails, return its error with the normal mapping.
6. Drop the owned `ModelRequest` after the first successful send, so that the
   conversation does not stay in memory for the whole stream.

The fallback happens before the stream yields any event, so the caller sees
one clean stream. A cancel during either send ends the stream with no event,
as today. The native rebuild cannot fail on the schema (section 7.6 is total),
so the only new error location is the second send.

**ADR-10.** ADR-10 says that the provider and the runner never retry
automatically; `RetryingModel` does that job. The fallback does not conflict
with it, for four reasons. It sends a *different* request, not the same one
again. It occurs at most once for each request. It is logged. It never repeats
after a transient error; throttling and server errors still go to the caller.

**Weak contract.** The match on error text can break if Bedrock changes the
text. The result is then the same as on `main`: a clear `ValidationException`.
Rule A still covers the known models.

### 7.5 Request shape on the `Native` path

| Caller input | `ForcedTool` (unchanged) | `Native` |
| --- | --- | --- |
| `JsonSchema { name, schema }` | Synthesized tool + `toolChoice: tool` | `outputConfig.textFormat` = `{type: json_schema, structure: {jsonSchema: {schema, name}}}`. No `toolChoice`. |
| `JsonSchema`, no user tools, history has `toolUse`/`toolResult` | Synthesized tool + `toolChoice: tool` | `outputConfig` as above, **plus the synthesized tool as a `toolConfig` anchor without `toolChoice`**. `synthesizing = true`. |
| `JsonObject` | Synthesized tool + `toolChoice: tool` | No `outputConfig`. Degrades to text. Native output cannot express a free-form object. |
| `ToolChoice::Required` (see section 8, question 1) | `toolChoice: any` | `toolChoice` omitted |
| `ToolChoice::Required`, no tools | Error | Error (same as today) |
| `ToolChoice::Tool { name }` (see section 8, question 1) | `toolChoice: tool` | `toolChoice` omitted. The name is still checked against the tool list. |
| `ToolChoice::Auto` | `toolChoice: auto` | `toolChoice: auto` |
| `ToolChoice::None` | `toolConfig` omitted | `toolConfig` omitted. A `JsonSchema` still sets `outputConfig` (and the anchor row applies). |
| `JsonSchema` + `ToolChoice::Tool` | Error (mutually exclusive) | No error. `outputConfig`; the forced choice is omitted. |
| `JsonSchema` + `ToolChoice::Required` | Synthesized tool forced; `Required` overridden | `outputConfig`; `Required` omitted |
| `JsonSchema` + `ToolChoice::None` | Error (mutually exclusive) | No error. `outputConfig` only (and the anchor row applies). |
| User tools + `JsonSchema` | User tools + synthesized tool, forced | User tools + `outputConfig`. The model can call user tools. |

**Why the anchor.** The core finalizing and repair turns send
`tools: Vec::new()` with the full conversation (`core/src/loop_state.rs:383-387`
and `557-561`). After a tool round, Bedrock requires a `toolConfig`. The
synthesized tool is the anchor. If the model calls it, `synthesizing = true`
remaps the call to `TokenDelta`. If the model answers in text, the translator
already passes text through as `TokenDelta`. A stub tool made from the history
also works, but a call to it would be a call to a tool that the core does not
know.

`schema` is the native schema (section 7.6), serialized to a JSON string,
because `JsonSchemaDefinition.schema` is a `String`. The `strict` flag on
`ResponseFormat::JsonSchema` has no effect on this path.

**Logs.** Each change of meaning (an omitted `Required`, an omitted `Tool`,
`JsonObject` degraded to text) logs one `warn!` for each handle and kind, not
for each request. Llama and Titan keep their current `debug!`.

### 7.6 Native output schema

Add a crate-private function, for example
`fn native_output_schema(schema: &Value) -> Value`. It starts from the
**caller's raw schema**, not from the `Ruleset::Strict` output. Strict collapses
`oneOf`/`anyOf`/`allOf` into one object and drops `required`. With
`additionalProperties: false` added, such a node accepts only `{}`.

The function is total and applies these rules to every subschema, also inside
`$defs`, `definitions`, `anyOf`, `allOf`, `items` and `properties`:

| Input | Output |
| --- | --- |
| An object node (`type: object`, a type array with `object`, or `properties` present) without `additionalProperties` | Add `additionalProperties: false` |
| `oneOf` | Rename to `anyOf` |
| `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`, `multipleOf` | Remove |
| `maxItems`, `uniqueItems` | Remove |
| `minItems` greater than 1 | Remove |
| `not`, `if`, `then`, `else` | Remove |
| Everything else | Keep |

**Policy: a denylist, not an allowlist.** The function removes only keywords
that are probed as rejected and that it can remove without a wrong result
(removing a limit makes the schema looser). It keeps all other keywords.

**Shapes with no safe rewrite pass through unchanged.** These are map types
(`additionalProperties: true` or a schema), `patternProperties`,
`propertyNames`, `prefixItems` (tuples), the boolean schema `true`, the empty
schema `{}`, and a recursive `$ref`. Bedrock then rejects the request with a
clear message (section 2.2). The provider does not change them, because each
possible change gives a wrong result. For example, `additionalProperties: false`
on a map makes it always empty.

The removed limits are not enforced by Bedrock. The core `validate_terminal`
checks the output against the caller's original schema, so a value that breaks
a removed limit gets the normal repair turn.

### 7.7 Capabilities

`caps_for(Anthropic)` stays unchanged: `structured_output: true`. On the
`Native` path, `JsonSchema` works, so the flag is true. `JsonObject` degrades to
text on these two models; the core flag has no separate value for it. The
README states this.

The premise of `structured_output_matches_forced_tool_choice_support` and the
module doc of `capabilities.rs` say that structured output equals forced tool
choice. Change both to say that structured output equals "forced tool choice
or native output". The family-level test stays valid.

### 7.8 Request plumbing

- `PreparedConverse` gets `output_config: Option<OutputConfig>`.
- `invoke` calls `.set_output_config(prepared.output_config)`.
- `to_wire_json` writes `outputConfig` **only when it is `Some`**, so that all
  existing snapshot files stay byte-identical. It reads the schema through
  `structure()`, not through `Debug`.
- Raise the workspace pin of `aws-sdk-bedrockruntime` from `1.135.0` to the
  lowest version that has all the types in use. `1.148.0` is the candidate.
  Verify it with `cargo update -p aws-sdk-bedrockruntime --precise <v>` and a
  build, then restore the lock to `1.157.0`.

### 7.9 Release

This is a `fix(providers)` change in one crate. It does not use new
`paigasus-helikon-core` API, so `core` gets no bump. Do not bump any version by
hand; release-plz bumps the crate and the facade.

## 8. Open questions for Sven

1. On the `Native` path, should `ToolChoice::Required` and `ToolChoice::Tool`
   be omitted with a `warn!` (current draft, the same as Llama and Titan), or
   return an error before the request is sent? Omitting keeps agents running
   but weakens a "must call a tool" contract. An error is honest but breaks
   callers that work on other Claude models.

## 9. Docs

- **Crate README**, family table: the `Anthropic` row says "Forced tool choice:
  yes". Change it to say yes, except Sonnet 5.5 and Opus 5.5. Add a section on
  the native path: which models use it, the runtime fallback, the schema rules
  of section 7.6, the shapes that Bedrock rejects, that removed limits are not
  enforced on the server, `JsonObject` degrading to text, and that user tools
  stay callable when `JsonSchema` is set.
- **Crate README**, line 77: it says that Bedrock rejects `$ref`/`$defs` and
  `anyOf` in general. Limit that sentence to tool input schemas.
- **mdBook**, `docs/book/src/concepts/model-providers.md`: the Bedrock
  paragraph (line 133) and "Family-gated structured output" (line 176) both say
  structured output is forced-tool synthesis on Anthropic. Add the native path
  for Sonnet 5.5 and Opus 5.5.
- `mdbook build docs/book` and `npx markdownlint-cli2` must stay clean.

## 10. Tests

### Existing tests and docs to change

- `json_schema_on_global_claude_profile_synthesizes` (`translate/mod.rs:574`)
  asserts a forced `toolChoice: tool` for `global.anthropic.claude-sonnet-5-5`.
  Change it to a model that keeps the forced path (for example
  `global.anthropic.claude-sonnet-5`), and add the native counterpart.
- `structured_output_matches_forced_tool_choice_support` and the
  `capabilities.rs` module doc (section 7.7).

### Unit tests (CI)

- `rejects_forced_tool_choice` is `true` for `us.`, `global.`, `eu.`, no-prefix
  and ARN forms of `claude-sonnet-5-5` and `claude-opus-5-5`. It is `false` for
  `claude-sonnet-5`, `claude-opus-5`, `claude-haiku-5-5`,
  `claude-sonnet-4-5-…`, Nova and Mistral IDs.
- Request-shape snapshots (`to_wire_json`) for a Sonnet 5.5 config, one for each
  row of section 7.5, including the anchor row. Each asserts that no
  `toolChoice` of type `any` or `tool` is present.
- A snapshot that a Sonnet 5 and a Haiku 5.5 config still send the synthesized
  tool and `toolChoice: tool` (anti-goal 3).
- `native_output_schema`: each row of section 7.6, nested objects, an array of
  objects, `$defs`, `type: ["object","null"]`. Also inputs from
  `schemars::schema_for!` for a unit enum with variant doc comments,
  `Option<Enum>`, `Option<Struct>`, a struct with `u32` fields, a `HashMap`
  field (passes through) and a recursive type (passes through).
- Classifier: both patterns match. Near misses do not match: other
  `ValidationException` messages, other error codes, and the `outputConfig`
  rejection texts.
- Policy: `path()` for each combination; `record_rejection()` returns `true`
  only once, also from two threads.
- Fallback flow, with fakes for the send closure: a rejection, then a second
  send with `outputConfig` and no forced choice; the flag is set; a second
  request on the same handle goes native at once; a non-Anthropic family does
  not fall back; other errors do not fall back; a second rejection is returned.

No new dev-dependency is necessary. If the builder wants a wire-level test, use
the local plain-HTTP server pattern in
`tests/provider-stream-conformance/src/{server,eventstream}.rs` (a real
`BedrockModel` with `endpoint_url`). That server can answer the first request
with a 400 error JSON. Do not add `aws-smithy-http-client` with `test-util`: an
AWS HTTP dev-dependency can add a second rustls crypto provider under
`--all-features`.

Run the exact CI gates: `cargo test --workspace --all-features`,
`cargo clippy --workspace --all-features --all-targets -- -D warnings`, and
`cargo build -p paigasus-helikon-providers-bedrock`.

### Live tests (manual, `#[ignore]`)

Run with the `maip-dev-admin` profile in `us-east-1`.

- **Acceptance:** `structured_output_synthesis` with
  `BEDROCK_MODEL_ID=global.anthropic.claude-sonnet-5-5` passes. Also run it
  with `us.anthropic.claude-opus-5-5` and with `us.anthropic.claude-sonnet-5`
  (forced path).
- A live test that sends a schemars-style schema (`u32` with `minimum`, a nested
  object without `additionalProperties`, an `Option<Enum>`) to Sonnet 5.5. It
  must return valid JSON.
- A live test of the finalizing-turn shape: a history with `toolUse` and
  `toolResult`, no tools, and `JsonSchema`, on Sonnet 5.5. It must return valid
  JSON.
- A live test of rule B, inside the crate (under `src/`, so that it can turn
  rule A off). It sends `JsonSchema` to Sonnet 5.5 and asserts one rejection,
  the flag set, and valid JSON.

## 11. Left to the builder

- Names of the types, functions and fields.
- The exact layout of the policy next to `Config`.
- Where `native_output_schema` lives (`schema.rs` or a new module).
- How `invoke` is restructured for the fallback, within section 7.4.
- The exact `warn!` and `debug!` text.
- Per-handle learning (the draft) is my call. A process-wide set keyed by model
  ID is possible later if short-lived handles make the rejected request a cost.
