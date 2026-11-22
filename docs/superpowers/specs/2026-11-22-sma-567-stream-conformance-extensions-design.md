# SMA-567: extend the stream conformance suite

Linear: [SMA-567](https://linear.app/smaschek/issue/SMA-567). Parent design:
`docs/superpowers/specs/2026-08-19-sma-533-stream-conformance-design.md` (the "SMA-533 design").
Suite: `tests/provider-stream-conformance`.

## 1. Intent

SMA-533 built a cross-provider suite that checks the order of `ModelEvent`s. It scoped itself to
its own assertion list and recorded five gaps. This change closes the gaps that captured traffic
can support today. It also makes the gaps that captured traffic cannot support yet visible and
pinned, so that they cannot disappear in silence.

The provenance rule of the suite stays in force without exception. A fixture is transcribed from
captured or already-committed captured traffic. It is never written from vendor documentation or
from translator code.

## 2. Goals, non-goals, anti-goals

**Goals.**

- G1. Nothing follows an `Err` (ticket item 3).
- G2. Reasoning deltas keep the channel order of the wire (ticket item 1).
- G3. Parallel tool calls keep their arguments apart (ticket item 2).
- G4. Every tool call's arguments parse as JSON, including a zero-argument call (ticket item 4).
- G5. The Bedrock `messageStop` / `metadata` wire order is pinned, and `CleanStop` has a
  positive-evidence floor for `Usage` (ticket item 3b, changed: see §3.2).
- G6. Each (subject, scenario) pair **for a scenario that this change adds** that has no fixture
  meeting the provenance rule is a pinned row in a new `UNCAPTURED` table, with a follow-up Linear
  issue.

**Non-goals.**

- No new live captures in this change. Option (A) of the brainstorm was rejected.
- No behaviour change to a provider, except a fix for a conformance failure that the suite finds
  (SMA-533 design §9 applies without change), and the Bedrock decision in §6.6.
- No new subject.
- G3 does not cover arguments of two calls that interleave on the wire (the SMA-550 / SMA-566
  bug class). Gemini, the only captured subject, sends each call whole in its own chunk. A capture
  with interleaved arguments is input to the parallel follow-up issue.

**Anti-goals.** The change fails, even if CI is green, when:

- a new fixture has no captured traffic behind it;
- a `DECLINED` row says "cannot occur" for a shape that can occur but has no capture;
- an `UNCAPTURED` row says "only a capture is missing" when a capture alone cannot let the subject
  serve the scenario;
- a new rule cannot be shown to fail by a fake or a perturbed sequence;
- a subject skips a new scenario without a pinned row.

## 3. Facts

### 3.1 Captured traffic for each new shape

| Shape | Captured, usable | Not captured |
| --- | --- | --- |
| Reasoning | `anthropic`: `thinking_text_tool_use.txt` (SMA-840 probe run C: thinking, text, `tool_use`) and `empty_thinking_tool_use.txt` (run A: one empty `thinking_delta`, then `tool_use`). | `openai/responses`: `responses_reasoning_then_text.txt` is hand-authored (SMA-316). The gpt-5-mini captures of 2026-11-21 (SMA-585) are not in the repo. `litellm`: inline JSON only. `bedrock`: typed SDK events only. `gemini`: the request never sets `includeThoughts` or `thinkingConfig`, so no thought part arrives on the wire (`providers-gemini/src/lib.rs:59-60`). `openai/chat`: the Chat Completions wire from api.openai.com carries no reasoning text. |
| Parallel calls | `gemini`: `parallel_signed.sse`, recorded as "Capture 1" in `docs/superpowers/specs/2026-11-20-sma-827-gemini-thought-signatures-design.md:299`. | `anthropic`: `parallel_tool_use.txt` came with SMA-317 (commit `d565b58f`) and has no capture behind it. `openai/chat`: `chat_parallel_tool_calls.txt` is hand-authored. `openai/responses`, `litellm`, `bedrock`: inline or typed events only. |
| Zero-argument call | `openai/responses`: `responses_tool_call_zero_args.txt`, captured from api.openai.com on 2026-08-22. The API streams **one** `{}` delta, not zero deltas. | All other subjects: hand-written inline tests only. A stream with truly zero argument deltas has no capture for any subject. |
| After `Err` | No new fixture. The rule runs on the existing scripts. | — |

The spec challenger ran both new captures through the current translators. Both pass every
existing assertion and the new rules in §6. Gemini `parallel_signed.sse` gives three calls with
native ids, one call-scoped `ProviderData` for each call, and `Finish(ToolCalls)`. The zero-argument
capture gives one `{}` delta, `Usage`, and `Finish(ToolCalls)`.

### 3.2 The ticket's 3b floor cannot catch the Bedrock swap, so this design does not add it

The ticket proposes a floor that requires `Usage` and `Finish` to be adjacent and terminal in
`CleanStop`, and says that it "would close" the Bedrock swap. It does not:

- When `metadata` comes before `messageStop`, `bedrock/src/stream.rs:253-280` emits `Usage` at
  once and `Finish` on the stop. The `ModelEvent` sequence is `…, Usage, Finish` in both orders.
- `openai/chat` and `litellm` buffer `Finish` until end-of-stream. A script with the usage chunk
  before the `finish_reason` chunk also gives the same events. So the floor cannot see the
  SMA-522 fixture mistake either.
- All six subjects already pass the floor on their current fixtures. It finds no defect.
- The core contract lets `Usage` appear anywhere. On a tool stop, the chat and LiteLLM `finish()`
  methods correctly flush a buffered name between `Usage` and `Finish`
  (`providers-openai/src/backend/chat.rs:519-529`, `providers-litellm/src/stream.rs:739-754`).

The floor adds a rule that is not in the contract, and no bug class justifies it. This design
replaces it with a byte-order check on the Bedrock scripts (§6.5), which does catch the swap, and a
positive-evidence floor: `CleanStop` must yield at least one `Usage`.

### 3.3 "Nothing after `Err`" conflicts with Bedrock today

`Model::invoke` (`core/src/model.rs`) says that `Finish` is terminal and that `Finish` must not
follow a mid-stream error. It says nothing about other events after an `Err`.

- **Bedrock emits `Usage` after an `Err`.** With `tool_choice: None`, the translator pushes an
  `Err` when the model calls a tool (`providers-bedrock/src/stream.rs:150-160`). It then emits
  `Usage` when `metadata` arrives. The driver yields every item from `consume` and does not stop
  after an inner `Err` (`providers-bedrock/src/model.rs:173-177`). The test
  `forbid_tool_calls_turns_a_tool_use_into_an_error` (`stream.rs:848-869`) pins `out[1]` as `Usage`.
- **Anthropic does not break the rule.** Its driver returns after `consume` returns `Err`
  (`providers-anthropic/src/model.rs:145-149`). The `refusal` `Err` (`stream.rs:383`) is produced at
  `message_stop`, the last event.
- **Gemini, OpenAI and LiteLLM** return after an `Err`. LiteLLM logs and skips an unparseable frame
  and emits no `Err`.
- No conformance scenario reaches the Bedrock path, so assertion 9 alone stays green.

§6.6 holds the decision that this requires.

### 3.4 The core accumulator's rule for arguments

`ModelTurnAccumulator::build_items` (`core/src/model.rs:640-653`) changes blank or whitespace-only
arguments to `{}`, then parses them as any `serde_json::Value`. Arguments that fail to parse make
`finish()` return an error. Gemini sends a missing `args` as `"null"` (`providers-gemini/src/sse.rs:45-52`),
which parses. Assertion 10 uses the same rule as the accumulator: parse as any JSON value.

### 3.5 Existing provenance gaps found on the way

For `ToolCallCleanStop`, the `anthropic` subject cites `parallel_tool_use.txt` and the
`openai/chat` subject cites `chat_parallel_tool_calls.txt` (`conformance.rs:537-539`, `1649-1653`).
Both files are hand-written (§3.1). This change does not fix that. The parallel follow-up issue
records it.

## 4. Approaches

1. **Extend the current machinery (chosen).** Add scenarios, `classify` rules, floors and fakes in
   the existing style. Add an `Outcome::Uncaptured` variant and an `UNCAPTURED` table that is
   pinned in both directions, the same as `DECLINED`.
2. **An opt-in trait for the new scenarios.** Rejected. A subject without a capture would not take
   part, so a skip would be invisible again. The pinned-in-both-directions check is the part of
   SMA-533 that prevents this.
3. **The new checks in each provider crate's tests.** Rejected. The checks would no longer be
   cross-provider, which is the purpose of the suite.

Sven chose approach 1 and option (C) (a separate `UNCAPTURED` table) on 2026-11-22.

## 5. Decisions

- D1. `DECLINED` keeps its SMA-533 meaning: the wire shape cannot occur for this subject.
- D2. A new pinned table, `UNCAPTURED`, holds the pairs where the shape can occur, a capture alone
  would let the subject serve it, and no capture exists. Each row names a follow-up Linear issue.
  Only the scenarios that this change adds may have `UNCAPTURED` rows (§6.7).
- D3. Bedrock has no wire captures at all. Its existing subject is derived from the SDK types, as
  SMA-533 accepted. For the new scenarios, Bedrock takes `UNCAPTURED` rows. New derived fixtures
  are not allowed.
- D4. Reasoning gets a new provider-neutral scenario, `ReasoningCleanStop`. The existing
  `ThinkingToolCallCleanStop` stays the Anthropic signed-thinking scenario, and its `DECLINED` rows
  do not change. (The first draft reused `ThinkingToolCallCleanStop`. The challenger showed that
  its floors need a call-scoped `ProviderData`, which three of the four subjects never emit, and
  that its script leaves out the text block. So the draft's `UNCAPTURED` rows were false.)
- D5. Assertion 9 and the floors are rules about fidelity to the wire and the contract. They are
  not rules about one provider. Where a rule is stricter than the contract, its doc comment says so.

## 6. Design

### 6.1 New scenarios

| Scenario | Shape | Served by | Declared `FinishReason` |
| --- | --- | --- | --- |
| `ReasoningCleanStop` | Reasoning, then text, then one tool call, then the stop reason. | `anthropic`, from all of run C (`thinking_text_tool_use.txt`) | `ToolCalls` |
| `ParallelToolCallCleanStop` | Two or more complete tool calls, then the stop reason. | `gemini`, from `parallel_signed.sse` | `ToolCalls` |
| `ZeroArgToolCallCleanStop` | One tool call with an empty argument object, then the stop reason. | `openai/responses`, from `responses_tool_call_zero_args.txt` | `ToolCalls` |

All three expect a stop reason and end with `Finish`. All three get the clean-stop floor (a
`Finish` exists and carries the declared reason). The two tool scenarios get the tool floors.

`ReasoningCleanStop` is "clean stop" in the sense of the suite: the stream ends after a stop
reason. Its name does not say `end_turn`. The only reasoning capture ends with `tool_use`, so the
scenario includes the tool call. The `anthropic` script for it keeps the text block of run C, at
index 1, and the `tool_use` at index 2. The text block emits a text-position `ProviderData` with
no `call_id` (`providers-anthropic/src/stream.rs:275-279`). Assertion 8 ignores it.

Add the three scenarios to `Scenario::ALL`, `expects_stop_reason`, `fakes::conforming`, the
`fakes::finish` reason mapping and the `declared_finish_reason` test helper.

### 6.2 New `classify` rules

These rules need no fixture knowledge, so they go in `classify` and run on every scenario.

**Assertion 9: nothing after `Err`** (G1). Any event, `Ok` or `Err`, after the first `Err` is a
violation: `Violation::EventAfterError`. Order inside `classify`: after assertion 6
(`FinishAfterError`), so that the more specific name wins for a `Finish` after an `Err`. The exact
form of the rule depends on the decision in §6.6.

**Assertion 10: tool arguments parse as JSON** (G4). Runs only when the stream contains a
`Finish`. A truncated, errored or cancelled stream can correctly end in the middle of the
arguments, and a consumer calls `finish()` only after a `Finish`. For each `call_id` group,
concatenate the `args_delta`s in stream order. Blank or whitespace-only becomes `{}`. The result
must parse as a JSON value: `Violation::ToolArgsNotJson { call_id }`. This is the accumulator's
rule (§3.4). A blank-`call_id` group is checked only when it carries exactly one name. With two
or more names, the group holds several calls, and the accumulator refuses that turn anyway
(SMA-621). Assertion 10 runs on every tool scenario for every subject, so it also covers the
existing fixtures. All existing served fixtures pass it today.

### 6.3 New floor checks

These checks need fixture knowledge, so they go in `floor_violation`, the same as the existing
`FinishReasonMismatch` check. The existing rule of `floor_violation` stays: a floor reports only
missing evidence, or a mismatch with a declaration. It leaves the exact diagnosis to `classify`.

**No `Err` outside the error scenarios.** In every scenario except `ErrorMidGeneration` and
`ErrorAfterStopReason`, the stream must yield no `Err`. Today, nothing stops a clean script from
being read as an error. This floor closes the SMA-533 §6 statement that S3 "will not be
mis-reported as S4a".

**`CleanStop` yields a `Usage`.** Every `CleanStop` script carries usage, so a stream without
`Usage` did not serve the scenario's bytes.

**Channel order** (G2). A new trait method declares the run order of the fixture for a scenario:

```rust
/// One run of same-channel events, in wire order.
pub enum Channel { Reasoning, Text, Tool }

fn fixture_channel_order(&self, scenario: Scenario) -> Option<&'static [Channel]>;
```

`None` means "no claim". For `ReasoningCleanStop`, the subject must return `Some(..)`. If it
returns `None`, the floor fails with `InsufficientEvidence`. The check maps each `ReasoningDelta`,
`TokenDelta` and `ToolCallDelta` to its channel, ignores all other events, merges adjacent equal
channels into one run, and compares the result with the declared order:
`Violation::ChannelOrderMismatch { expected, observed }`. A further floor for `ReasoningCleanStop`
requires at least one `ReasoningDelta`.

The contract lets the three channels interleave freely (`core/src/model.rs:56-57`). This check is
therefore about fidelity to the wire order, not about the contract. Its doc comment says so. An
empty delta still counts as a run. Run A shows that a real wire can carry an empty
`thinking_delta`, and the translator passes it on as an empty `ReasoningDelta`.

The `anthropic` declaration for `ReasoningCleanStop` is `[Reasoning, Text, Tool]`, read from run C.

**Declared calls** (G3, G4). A new trait method declares the calls in the fixture:

```rust
/// A tool call the fixture carries: its name and its complete arguments as JSON text.
pub struct ExpectedCall { pub name: &'static str, pub args: &'static str }

fn fixture_calls(&self, scenario: Scenario) -> &'static [ExpectedCall];
```

`args` is a `&'static str` so that the table can be a `const`. The floor parses it.
`fixture_calls` must return a non-empty list for `ParallelToolCallCleanStop` and
`ZeroArgToolCallCleanStop`, and an empty list for every other scenario. The existing tool
scenarios keep `fixture_tool_name`, which does not change. For the two new scenarios, the
"declared name is present" floor reads the names from `fixture_calls`. So the Gemini names
(`search`, `weather`) and the zero-argument name (`get_current_time`) need no change to
`fixture_tool_name`.

For both new scenarios:

- the observed calls, in first-seen `call_id` order, must equal the declared calls by name and by
  parsed arguments: `Violation::ToolCallsMismatch { expected, observed }`;
- a call whose arguments do not parse is skipped by this floor, so that assertion 10 reports it
  with its exact name;
- the match is by position, not by `call_id`, because a subject cannot always declare the ids
  that its translator makes.

For `ParallelToolCallCleanStop`, a further floor requires two or more distinct non-blank
`call_id`s. For `ZeroArgToolCallCleanStop`, the declared call is `get_current_time` with arguments
`{}`. So a fixture with real arguments fails the floor, and the scenario cannot pass vacuously.

### 6.4 `Outcome::Uncaptured` and the `UNCAPTURED` table

```rust
pub enum Outcome {
    Served { .. },
    Declined(&'static str),
    /// The wire shape can occur for this provider, and a capture alone would let the subject
    /// serve it, but no fixture that meets the provenance rule exists. The reason must match the
    /// pinned `UNCAPTURED` table.
    Uncaptured(&'static str),
}

/// (subject, scenario, reason, follow-up Linear issue)
pub const UNCAPTURED: &[(&str, Scenario, &str, &str)] = &[ .. ];
```

An `Uncaptured` outcome is not served. No floor or assertion runs for it, the same as `Declined`.

`assert_conforms` collects uncaptured pairs separately from declined pairs and checks them against
`UNCAPTURED` in both directions, the same as `assert_declines_match`: an unpinned pair, a stale row
and a changed reason each fail. The two outcomes are not interchangeable. A subject that reports
`Uncaptured` for a pair that `DECLINED` lists fails, and the reverse also fails.

Static unit tests in `declines.rs`:

1. No pair appears in both tables.
2. Every subject in `UNCAPTURED` is in `REGISTERED_SUBJECTS` (extend
   `every_declined_subject_is_registered`).
3. Every follow-up field matches `^SMA-\d+$`.
4. For each scenario in `Scenario::ALL`, at least one registered subject is in neither table. So
   no scenario can end up served by nobody.
5. `UNCAPTURED` rows name only scenarios from an explicit allow-list, which holds the three new
   scenarios. Moving an SMA-533 scenario into `UNCAPTURED` then needs a reviewed edit to that list.

Each follow-up issue states in its acceptance criteria that the issue closes only when its rows
are removed: by a capture, or by a reviewed move to `DECLINED`.

### 6.5 The Bedrock wire-order check

This is G5. Add a function in the `bedrock` module of `tests/conformance.rs` that takes a chunk
list and fails when a `metadata` frame comes before a `messageStop` frame. It finds each frame by
its `:event-type` header. If `src/eventstream.rs` has no header reader, add a minimal one in the
test code. It must decode the header block, not copy the encoder.

- Run the function on every Bedrock script that has both frames (`CleanStop`, `ToolCallCleanStop`
  and any other).
- Unit-test the function on a swapped list, so that the proof that it can fail stays in the code.
- Replace the "do not reorder it casually" prose in the `metadata()` doc (`conformance.rs`,
  near lines 168-181) with a pointer to the check. Keep the reason in the prose.

### 6.6 OPEN: the contract for events after an `Err`

This needs Sven's decision before implementation (§3.3). The options:

- **A. An `Err` is terminal without exception.** Add to the `Model::invoke` ordering list, after
  "`Finish` is the terminal event; nothing follows it.":

  > - An `Err` item is terminal too: implementations MUST NOT yield any item after it.

  The Bedrock driver then stops after it yields an inner `Err`. The `Usage` that comes after the
  `tool_choice: None` error is lost, and `forbid_tool_calls_turns_a_tool_use_into_an_error`
  changes. Assertion 9 is strict.
- **B. An `Err` is terminal, except for a trailing `Usage`.** The sentence allows `Usage` events
  after an `Err` and nothing else. Assertion 9 exempts `Usage`. Bedrock does not change.

In both cases, add a translator-level test in `providers-bedrock`, because no conformance scenario
reaches this path.

The contract sentence is a doc-only change to `paigasus-helikon-core`. It goes in its own commit,
`docs(core): SMA-567 …`. No `version` field is edited by hand. release-plz then gives `core` a
patch bump and the dependants get the facade cascade. The PR body says this. The CHANGELOG entry
and the book describe it as a contract clarification, the same as SMA-533 §10. Update
`docs/book/src/concepts/model-providers.md` (the paragraph that describes `Finish` as terminal) to
match.

### 6.7 Rows in the pinned tables

The reasons below are proposals. The builder may change their wording, but not their meaning.

**`DECLINED`, new rows.**

| Subject | Scenario | Reason |
| --- | --- | --- |
| `gemini` | `ReasoningCleanStop` | the request never enables thought parts, so none arrive on the wire |
| `openai/chat` | `ReasoningCleanStop` | the Chat Completions wire carries no reasoning text |

The `ThinkingToolCallCleanStop` rows do not change.

**`UNCAPTURED`, new rows.** Three follow-up issues, one for each shape:

| Scenario | Subjects | Follow-up issue |
| --- | --- | --- |
| `ReasoningCleanStop` | `bedrock`, `litellm`, `openai/responses` | capture a reasoning stream for each subject. The gpt-5-mini captures (SMA-585) are the first input. |
| `ParallelToolCallCleanStop` | `anthropic`, `bedrock`, `litellm`, `openai/chat`, `openai/responses` | capture a parallel tool-call stream, if possible with interleaved arguments. Also record §3.5. |
| `ZeroArgToolCallCleanStop` | `anthropic`, `bedrock`, `gemini`, `litellm`, `openai/chat` | capture a zero-argument tool-call stream, including a stream with no argument deltas. Note the Gemini `"null"` arguments (§3.4). |

The builder files the three issues in the Linear `Paigasus Helikon` project before writing the
table, so that the table cites real identifiers.

### 6.8 Proving that each new rule can fail

This is the acceptance criterion "each new assertion has a non-conformant fake".

`conforming()` changes:

- `ReasoningCleanStop`: a `ReasoningDelta`, then the text, then one tool call, then `Usage` and
  `Finish(ToolCalls)`.
- `ParallelToolCallCleanStop`: two calls with two different `call_id`s. `fakes::tool` takes a
  `call_id` parameter.
- `ZeroArgToolCallCleanStop`: one call with one `{}` delta.

New `Fake` variants for the `classify` rules, each in `every_fake_is_rejected_with_its_classification`:

- `Fake::TokenAfterError`: append a `TokenDelta` to an error scenario. Rejected as
  `EventAfterError`.
- `Fake::ArgsNotJson`: replace the tool deltas with a truncated `{"city":`. Rejected as
  `ToolArgsNotJson`.
- `Fake::ProviderDataUnknownCall`: a call-scoped `ProviderData` for an id that no call used.
  Rejected as `ProviderDataUnknownCall`. Assertion 8 has no fake today.

A positive test: a call with blank arguments (no `args_delta` text) passes assertion 10.

The fakes never reach the floors (`fakes.rs` module doc). For each new floor, add a test that
calls `floor_violation` directly with `conforming(scenario)` changed in one place, and asserts the
exact violation:

- a reasoning run moved after the text run: `ChannelOrderMismatch`;
- no `ReasoningDelta`: `InsufficientEvidence`;
- a `None` channel order for `ReasoningCleanStop`: `InsufficientEvidence`;
- the arguments of the two calls swapped: `ToolCallsMismatch`;
- one `call_id` only in the parallel scenario: `InsufficientEvidence`;
- the zero-argument call with real arguments: `ToolCallsMismatch`;
- an `Err` in `CleanStop`: the no-`Err` floor;
- no `Usage` in `CleanStop`: `InsufficientEvidence`.

`the_conforming_fake_clears_every_floor` covers the three new scenarios.

For the `UNCAPTURED` cross-check, add the same three negative tests that the `DECLINED`
cross-check has: an unpinned pair, a stale row and a changed reason.

### 6.9 Fixtures

Each new fixture is transcribed into `tests/conformance.rs`, the same as the existing fixtures. It
has a provenance comment that names the source file, the capture that the source file records,
and every edit. The SMA-533 rules for transcription apply: no invented events, no reordered
events, and each removed event is named.

- `anthropic` `ReasoningCleanStop`: all of run C (`thinking_text_tool_use.txt`). Keep the text
  block. The signature stays replaced, as in the source file.
- `gemini` `ParallelToolCallCleanStop`: from `providers-gemini/tests/fixtures/parallel_signed.sse`
  (three calls and a fourth `STOP` chunk). Cite SMA-827 design "Capture 1", because the file has
  no provenance header. Three known traps:
  1. The capture writes `"finishReason": "STOP"` with a space. The `encodes_stop_reason` marker
     (`conformance.rs`, near line 2485) has no space. Either the marker check tolerates the space,
     or the transcription records the change of whitespace as an edit.
  2. `tool_call_chunk` (`conformance.rs`, near lines 2575-2596) always adds `thoughtSignature`.
     Only call 1 of the capture has one. Use a builder that can leave it out.
  3. `request()` must offer the tools `search` and `weather`.
- `openai/responses` `ZeroArgToolCallCleanStop`: from
  `providers-openai/tests/fixtures/responses_tool_call_zero_args.txt`. Keep its header's list of
  edits. `request()` must offer `get_current_time`.

### 6.10 Documentation

These doc comments become false and must be updated in this change:

- `floor_violation` (`lib.rs`, near lines 507-523): the new floors.
- `fixture_finish_reason` (`lib.rs`, near line 204): "Exactly four scenarios end with a `Finish`"
  becomes seven.
- `assert_conforms`: it names only `DECLINED`.
- The `conformance.rs` header, rule 1: it names only `declines.rs`.
- The `anthropic` module doc (`conformance.rs`, near lines 1662-1666): "this suite has no
  reasoning-delta scenario".
- The Bedrock `metadata()` doc (§6.5).

Other documentation:

- The SMA-533 design is not edited. This design is the record.
- Book: only the contract change in §6.6.
- READMEs: no change. The suite is not a published crate. The only published-crate change is a
  doc sentence in `core` (and, with option A, a Bedrock behaviour change that no README
  describes). This is a deliberate decision, not a skip.

## 7. Handling failures that the suite finds

The SMA-533 design §9 applies without change. A small, clear defect is fixed in this change and
noted in the PR body. A defect that needs a design decision stops the work and goes to Sven.

The first implementation task is a probe. It runs every subject against every new rule, before any
rule is committed as a test, and reports the results. The challenger predicts that all subjects
pass. The probe confirms this. Any surprise goes to Sven before the work continues.

## 8. Left to the builder

- The exact names of the new `Violation` variants and `Fake` variants.
- Whether `Channel` and `ExpectedCall` live in `lib.rs` or in a new module.
- The wording of each table reason, within §6.7.
- The task order after the probe. The suggested order is: the tables and `Outcome::Uncaptured`,
  then assertions 9 and 10, then the floors, then the three new scenarios, then the Bedrock check.
- The commit scopes, checked against `.versionrc`. The core contract sentence is a separate
  `docs(core)` commit.

## 9. Acceptance criteria

1. Assertions 9 and 10 and every new floor each have a perturbed sequence that is rejected with
   the exact violation (§6.8).
2. Each new (subject, scenario) pair is served, or is in `DECLINED` or `UNCAPTURED`. Both tables
   are pinned in both directions, and the five static tests of §6.4 pass.
3. Each new fixture cites captured traffic (§6.9).
4. The Bedrock wire-order check runs on every Bedrock script with both frames, and a unit test
   shows that it fails on a swapped list.
5. The §6.6 decision is implemented, with a translator-level Bedrock test.
6. Every CI gate in `CLAUDE.md` is green, including `test` on all three operating systems.
7. Three follow-up Linear issues exist and `UNCAPTURED` cites them.
