# SMA-565: Remove the retired Gemini 1.x and 2.x model ids — design

Linear: [SMA-565](https://linear.app/smaschek/issue/SMA-565). Size: a small change.

## Intent

A new Gemini Developer API user copies an example from our docs, or runs the
live tests. Today most examples name `gemini-2.5-flash`, and the Developer API
returns HTTP 404. The 404 looks like an auth problem or a provider bug, not like
a stale model id.

Sven decided on 2026-11-22: **the SDK does not support Gemini 1.x or 2.x.** The
Developer API no longer serves them to new users (2.0 to nobody). So no 1.x or
2.x id stays in code, tests or docs. The code path that exists only for those
ids goes too.

An existing key may still call a 2.5 model. After this change, such a user gets
the current request shape (which can include the dummy signature, never tested
on 2.x) and the conservative capability set. The CHANGELOG upgrade note tells
them this (see "Release and docs").

After this change, every Gemini id in the docs works for a new Developer API key.
The capability table gives correct values for the model that the docs name.

We have no Vertex credentials, so no Vertex claim here is live-tested.

## Facts (live probe, Developer API, 2026-11-21)

| Model id | Result |
| --- | --- |
| `gemini-2.5-flash`, `gemini-2.5-pro` | HTTP 404, "no longer available to new users" |
| `gemini-2.0-flash`, `gemini-2.0-flash-lite` | HTTP 404, "no longer available" |
| `gemini-3.6-flash`, `gemini-3.8-flash` | HTTP 200 |

The 404 text for `gemini-2.5-flash` says "Please update your code to use
models/gemini-3.8-flash". So Google itself names `gemini-3.8-flash` as the
replacement.

On `gemini-3.8-flash` these live tests passed through the SDK:
`live_developer_structured_output`, `live_developer_parallel_round` and
`live_agent_structured_output_with_tool`. A direct `generateContent` call with
an inline PNG image returned the correct colour. The `parallel_signed.sse`
fixture is a real `gemini-3.8-flash` response that contains three parallel
calls. (`live_developer_parallel_round` proves only that the API accepts a
history with parallel calls. The fixture is the evidence that the model emits
them.)

Core does not read capability flags (no `capabilities()` call in
`crates/paigasus-helikon-core/src`). The flags are advisory values that
`Model::capabilities()` reports.

## Acceptance criteria

- **AC1 ("the default model id resolves for a new API key").** SMA-827 already
  changed the Developer API live default (`tests/live.rs:18`) to
  `gemini-3.8-flash`. This PR changes the Vertex live default (`live.rs:197`)
  and every doc example.
- **AC2 ("any other occurrence of the retired id is updated or removed").** Read
  literally: no `gemini-1.*` or `gemini-2.*` id stays outside the historical
  `docs/superpowers/` files and `CHANGELOG.md` files.

## Goals

1. Every user-facing example names `gemini-3.8-flash`. This covers the crate
   `README.md`, the `lib.rs` rustdoc and `docs/book/src/concepts/model-providers.md`
   (Developer API and Vertex examples).
2. The README Vertex section and the book Vertex section
   (`model-providers.md:372`, which today says only "Model ids are
   illustrative") both say the same thing: model ids are examples, and Vertex
   model availability differs by region.
3. `tests/live.rs` has one `const DEFAULT_MODEL_ID: &str = "gemini-3.8-flash";`.
   `dev_model()` and `live_vertex_text_turn` both use it. The module doc says
   that the Vertex default is not live-tested. `live_developer_text_turn`
   unwraps each stream event (as `live_vertex_text_turn` does at `live.rs:206`),
   so an HTTP error fails the test with its error text. Today it drops every
   `Err` event at `live.rs:42`.
4. `KNOWN_MODELS` has one row only: `("gemini-3.8-flash", full())`. The four
   2.x rows go.
5. `is_gemini_3_or_later` goes. Every request gets the current behaviour: the
   dummy thought signature where a current-turn call has no valid signature, and
   the "Continue." user turn after a trailing model turn. No id-based branch
   stays in `items_to_contents`.
6. Every test that uses a 1.x or 2.x id uses `gemini-3.8-flash`, or goes if it
   tests only the removed 2.x branch.
7. Docs that describe the 1.x/2.x behaviour say instead that Gemini 1.x and 2.x
   are not supported. The support statement also goes in the README intro (or
   its Transports section) and in the book Gemini section. There it also says
   that an id with no `KNOWN_MODELS` row reports the conservative capability
   set, and that `.with_capabilities()` (`builder.rs:164`) overrides it.

## Non-goals

- No build-time error or warning for a 1.x or 2.x id. The SDK does not check
  model ids against a list. An unknown id gets the conservative capabilities and
  the current request shape, and the Gemini API gives its own error.
- No change to the historical files under `docs/superpowers/`, and no change to
  existing `CHANGELOG.md` entries. (The new upgrade note is added under
  `## [Unreleased]`.)
- No change to comments that record a past live capture (for example the
  `gemini-3.6-flash` capture note in `tests/provider-stream-conformance`).
- No new live tests and no Vertex live run.
- No `HELIKON_REQUIRE_GEMINI` flag. The negative control in Verification proves
  that the live run reached the API.
- Claude 3.x ids are out of scope. They are tracked in
  [SMA-850](https://linear.app/smaschek/issue/SMA-850).
- No manual version bumps. release-plz bumps the crate and the facade.

## Anti-goals

- A capability row must not claim a feature that we did not observe.
- A snapshot must not change in any way other than the one this design
  expects (see the snapshot decision below).

## Approach

Sven picked option A at the first question: change the ids and docs, and also
fix `KNOWN_MODELS`. Option B (ids and docs only, capability table in a new
issue) was rejected, because the docs would then name a model that reports wrong
capabilities.

At the spec gate, Sven decided that no 2.x id stays, because the models are not
supported. He then picked "remove the check" over "keep the check with 2.x test
ids". The Claude 3.x cleanup goes to a new issue (SMA-850), not this PR.

## Changes, file by file (`crates/paigasus-helikon-providers-gemini` unless stated)

- **`src/translate/request.rs`**
  - Delete `is_gemini_3_or_later` and its two call sites (the
    `add_dummy_signatures` call and the trailing-turn push run always).
  - Update the `TRAILING_MODEL_TURN_PROMPT` doc comment (`request.rs:412`,
    "for Gemini 3 and later") so it has no generation condition. Change
    "Gemini 3 starts the current turn" (`request.rs:282`) to "Gemini starts
    the current turn".
  - Keep the `model_id` parameter of `items_to_contents`.
    `Pending::valid_signature` uses it (`request.rs:166`): a signature from
    another model id counts as not valid.
  - Tests: delete `gemini_2_history_gets_no_dummy`,
    `gemini_2_real_signature_is_still_sent_back` and
    `gemini_2_trailing_model_turn_is_unchanged`. Rename
    `gemini_3_and_unknown_ids_keep_the_dummy` to
    `every_model_id_gets_the_dummy` and keep its ids
    (`models/gemini-3.8-flash`, `gemini-4-pro`, `my-tuned-model`). Rename
    `gemini_3_trailing_model_turn_gets_a_user_turn` (`request.rs:1719`) to
    `trailing_model_turn_gets_a_user_turn`. The
    `gemini-3.6-flash` id in `signature_from_other_model_or_api_gets_dummy`
    stays: it is "another model id", and 3.6 is a current model.
- **`src/translate/mod.rs`** — `snap::cfg()` uses `gemini-3.8-flash`.
- **Snapshots (`src/translate/snapshots/`)** — the exact expected change
  (verified by the second spec challenge): 2 of the 9 files change.
  `snap_tool_call_and_result` gains 1 line and
  `snap_parallel_same_name_tool_calls` gains 2 lines, each
  `"thoughtSignature": "skip_thought_signature_validator"`. The line before
  each new line gains a trailing comma (`}` becomes `},`). No snapshot gains a
  "Continue." turn, because none ends with a model turn. The other 7 files do
  not change. Run the snapshot tests, compare each `.snap.new` diff with this
  description, and accept only a diff that matches it. Any other diff is a
  stop: report it. No `.snap.new` file stays in the tree.
- **`src/capabilities.rs`** — `KNOWN_MODELS` has only the
  `gemini-3.8-flash` row. Replace `known_25_flash_…` with a test that pins
  that row: streaming, tools, parallel tool calls, structured output and vision
  are `true`, and `reasoning` is `false` (reasoning streaming is deferred, item
  D3). Add a test that `KNOWN_MODELS` has no duplicate ids, as the Anthropic and
  OpenAI crates do.
- **`src/builder.rs`, `src/model.rs`, `src/transport.rs`** — test ids
  `gemini-2.5-flash` and `gemini-2.5-pro` become `gemini-3.8-flash`, with the
  expected URLs and `model()` values updated in the same test.
- **`tests/gemini_wire.rs`** — ids and wiremock paths become
  `gemini-3.8-flash`. `vertex_url_and_bearer_header` gets a stronger check: no
  event is an `Err`, and the text "hi" arrives, as in the Developer API wire
  test. Today a wrong URL (wiremock 404, one `Err` event) still passes.
- **`tests/gemini_streaming.rs`** — ids change in place, including the
  `data[0].1["model"]` assertion. Do not add or remove lines above line 86,
  because `tests/provider-stream-conformance/tests/conformance.rs:2205-2213`
  cites lines in that file.
- **`tests/live.rs`** — Goal 3.
- **`src/lib.rs` (examples at 30 and 40, prose at 63-69), `README.md`** —
  examples use `gemini-3.8-flash`. Rewrite the signature paragraph
  (`README.md:98`, `lib.rs:63-69`) so that it has no generation condition:
  remove "The dummy is sent only for Gemini 3 and later ids. Gemini 1.x and 2.x
  requests keep their earlier shape and get no dummy." (README) and "It sends
  the dummy only for Gemini 3 and later ids" (`lib.rs:65`), and drop "For
  Gemini 3 and later," before the "Continue." sentence. In the same edit,
  correct the dummy rule to match the code (`request.rs:431-437`, test
  `mixed_content_is_sent_as_received`): the dummy goes on each `functionCall`
  part of a current-turn model content only when **no** call in that content
  has a valid signature. Add "Gemini 1.x and 2.x models are not supported." In
  the limits list, the dummy bullet becomes "it is live-tested only on
  `gemini-3.8-flash` (Developer API). It is not tested on Vertex." Add the
  Vertex region note (Goal 2) and the support statement (Goal 7).
- **`docs/book/src/concepts/model-providers.md`** — the same text changes as
  the README, at lines 294-372 and 451.
- **`CHANGELOG.md` of the gemini crate** — add `### Upgrade notes` under
  `## [Unreleased]` (precedent: the 0.3.0 entry in the same file). It says:
  Gemini 1.x and 2.x models are not supported; a request for any model id now
  gets the dummy thought signature (when no call in a current-turn model
  content has a valid signature) and the "Continue." turn after a trailing
  model turn; `gemini-2.5-*` and `gemini-2.0-*` ids now report the conservative
  capability set and `gemini-3.8-flash` reports the full set; a user who still
  calls a 2.x model should stay on 0.3.1.
- **`tests/provider-stream-conformance/tests/conformance.rs`** — `MODEL_ID`
  (`gemini-2.0-flash`) becomes `gemini-3.8-flash`. Replace the sentence that
  cites the task brief with: "Uses the id the crate docs name. `PacedServer`
  ignores the request, so this id only has to be one the crate accepts." The
  conformance requests are single-turn with no tool history, so the removed
  check does not change their bodies.

## Verification

1. `git grep -nE 'gemini-[12]\.' -- ':!docs/superpowers' ':!**/CHANGELOG.md'`
   returns nothing.
2. `git grep -nE 'is_gemini_3_or_later|starts_with\("gemini-|Gemini 3 and later|earlier shape' -- crates/paigasus-helikon-providers-gemini docs/book`
   returns nothing. (Step 1 cannot see the predicate, because
   `starts_with("gemini-1")` has no dot, and no remaining test fails if the
   branch stays.)
3. The full CI gate set from `CLAUDE.md` passes, plus `mdbook build docs/book`.
   No `.snap.new` file exists.
4. Live run, negative control: `GEMINI_MODEL_ID=gemini-2.5-flash cargo test -p
   paigasus-helikon-providers-gemini --test live -- --ignored --nocapture
   live_developer_text_turn` must **fail**, and its output must contain
   `gemini http 404`. This proves that the run reached the API.
5. Live run, default: the same command for all Developer API tests, with no
   `GEMINI_MODEL_ID`, must pass. Then run `cargo test -p
   paigasus-helikon-providers-gemini --lib -- --ignored --nocapture
   live_signature_round_trip`; it must pass, and its output must not contain
   "skipped". If `live_agent_two_sequential_tool_rounds` fails, run it once
   more. If it fails again, stop and report.
6. The Vertex default is not verified. The PR body says so.

## Release and docs

- Commit type `fix(providers)`, not `fix!`. release-plz gives
  `paigasus-helikon-providers-gemini` a patch bump and cascades to the facade.
  No public item changes (`KNOWN_MODELS` and `is_gemini_3_or_later` are
  crate-private), so cargo-semver-checks sees no change. My call: the models
  are unsupported, and a `!` would start the breaking cascade for no API
  change. The CHANGELOG upgrade note carries the behaviour change, because
  release-plz writes only commit subjects to the CHANGELOG.
- The squash commit body repeats the upgrade note.
- The book and the crate README change in the same PR (user-facing).
