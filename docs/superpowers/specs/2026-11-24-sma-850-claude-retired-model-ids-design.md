# SMA-850: Remove the retired Claude 3.x model ids — design

Linear: [SMA-850](https://linear.app/smaschek/issue/SMA-850). Size: a small change.
Precedent: SMA-565 (the same cleanup for the Gemini 1.x and 2.x ids,
`2026-11-21-sma-565-gemini-retired-model-ids-design.md`).

## Intent

A user copies the Bedrock example from our README or the book. Today it names
`anthropic.claude-3-5-sonnet-20241022-v2:0`. That model is retired, and the
error the user gets does not say "stale model id".

Sven decided on 2026-11-22: **the SDK does not support retired models.** So no
Claude 3.x id stays in code, tests or docs, and the capability rows for those
models go.

## Facts (live, Bedrock `us-east-1`, profile `maip-dev-admin`, 2026-11-24)

`aws bedrock list-foundation-models --by-provider anthropic` lists **no** Claude
3.x model. In `us-east-1`, every listed Claude model supports only the
`INFERENCE_PROFILE` inference type. So in that region, a bare `anthropic.…` id
cannot be invoked on demand.

| Id | Listing status |
| --- | --- |
| `anthropic.claude-sonnet-5-5`, `global.anthropic.claude-sonnet-5-5` | ACTIVE |
| `anthropic.claude-sonnet-4-6`, `us.`/`global.` profiles | ACTIVE |
| `anthropic.claude-haiku-4-5-20251001-v1:0`, `us.`/`global.` profiles | ACTIVE |
| `anthropic.claude-sonnet-4-5-20250929-v1:0` | LEGACY |
| `us.anthropic.claude-3-haiku-…`, `us.anthropic.claude-3-sonnet-…` profiles | ACTIVE profile, model not listed |

A direct `bedrock-runtime converse` call (`maxTokens=5`) gave these results:

| Id | Result |
| --- | --- |
| `anthropic.claude-3-5-sonnet-20241022-v2:0` | `ResourceNotFoundException`: "This model version has reached the end of its life." |
| `us.anthropic.claude-3-haiku-20240307-v1:0` | the same `ResourceNotFoundException` text, although the profile is listed ACTIVE |
| `global.anthropic.claude-sonnet-5-5` | HTTP 200, text "Ok" |

So "retired" is correct for every Claude 3.x id, including the ones whose
profile is still listed.

The only id-based behaviour rule in the Bedrock crate is
`family::rejects_forced_tool_choice` (`family.rs:147`). It matches
`claude-sonnet-5-5` and `claude-opus-5-5` only. `ForcedChoicePolicy::new`
(`policy.rs:39`) uses it to select the native output path. The Anthropic crate
uses the id only for the `KNOWN_MODELS` lookup (`builder.rs:210`) and for the
request `model` field (`translate/mod.rs:98`).

## Acceptance criteria

| Linear AC | How this PR meets it | Evidence |
| --- | --- | --- |
| AC1: every user-facing example names a current id that works on a new account | Goals 1 and 2 change every Bedrock example that names a 3.x or LEGACY id. Every Anthropic example already names a current id (`claude-sonnet-4-6`, `claude-sonnet-5-5`, `claude-haiku-4-5`). | Verification 1, 4 and 5. The "new account" part is **not** tested: all live runs use an existing dev account. The PR body says so. |
| AC2: no retired 3.x id stays outside `docs/superpowers/` | Goals 1, 3 and 4 | Verification 1 |
| AC3: capability rows for retired models are removed | Goal 3 | Verification 1 and 2 |
| AC4: the replacement ids are verified live | Bedrock: the Facts table, and Verification 3 and 4. Anthropic API: Verification 5. | See Verification 5 for the open decision. |

## Goals

1. Every user-facing Bedrock call example names `global.anthropic.claude-sonnet-5-5`
   (Sven picked this at the first question). Sites: `README.md:31`,
   `src/lib.rs:11`, `src/builder.rs:42`, `:57` and `:388` (rustdoc), and
   `docs/book/src/concepts/model-providers.md:171` and `:507`.
   Two detection examples are different:
   - The `family.rs:57` doctest uses `anthropic.claude-haiku-4-5-20251001-v1:0`.
     Line 61 already asserts `global.anthropic.claude-sonnet-5-5`, so the
     doctest keeps a bare-id case.
   - The book's prefix example (`model-providers.md:246`, today the LEGACY
     `global.anthropic.claude-sonnet-4-5-20250929-v1:0`) uses
     `global.anthropic.claude-sonnet-5-5`.
2. The Bedrock README and the book Bedrock section get two sentences, one idea
   each: "The example uses a cross-region inference profile id (`global.`).
   Current Claude models on Bedrock need an inference profile id in place of a
   bare `anthropic.` id." Then a third sentence: "Claude 3.x models are not
   supported." The Anthropic README and the book Anthropic section get the
   same support sentence, and one sentence that an id with no `KNOWN_MODELS`
   row reports the conservative capability set, which `.with_capabilities()`
   overrides.
3. `KNOWN_MODELS` in the Anthropic crate loses its 8 Claude 3.x and 3.5 rows and
   their two section comments. The Claude 4 rows stay unchanged.
4. Every test that uses a 3.x id as an arbitrary string uses a current 4.x id
   instead, so no test changes its code path (see "Test ids").
5. `family.rs` keeps every parsing case: base id with a version suffix, the
   `us.`, `apac.` and `us-gov.` prefixes, the foundation-model ARN and the
   inference-profile ARN. Only the model part of each id changes.
6. The developer doc of the Bedrock live suite (`tests/live.rs:6`, today the
   LEGACY Sonnet 4.5 id) names `global.anthropic.claude-sonnet-5-5`.
7. The Anthropic crate CHANGELOG gets an `### Upgrade notes` entry under
   `## [Unreleased]` (see "Release").

## Test ids

The test ids must not be `claude-sonnet-5-5` or `claude-opus-5-5`. Those ids
switch on the native output path, so the tool-choice tests and snapshots would
change behaviour, not only the id.

| Site | Old id | New id |
| --- | --- | --- |
| Bedrock offline tests (`builder.rs`, `fallback.rs`, `model.rs`, `translate/mod.rs`) | `anthropic.claude-3-5-sonnet-20241022-v2:0` | `anthropic.claude-sonnet-4-6` |
| `family.rs:164` (base id with version suffix) | `anthropic.claude-3-5-sonnet-20241022-v2:0` | `anthropic.claude-haiku-4-5-20251001-v1:0` |
| `family.rs:165` (`us.` profile) | `us.anthropic.claude-3-7-sonnet-20250219-v1:0` | `us.anthropic.claude-sonnet-4-6` |
| `family.rs:166` (`apac.` prefix) | `apac.anthropic.claude-3-5-sonnet-20241022-v2:0` | `apac.anthropic.claude-haiku-4-5-20251001-v1:0` |
| `family.rs:221` (`us-gov.` prefix) | `us-gov.anthropic.claude-3-5-sonnet-20240620-v1:0` | `us-gov.anthropic.claude-haiku-4-5-20251001-v1:0` |
| `family.rs:237` (foundation-model ARN) | `…/anthropic.claude-3-5-sonnet-20241022-v2:0` | `…/anthropic.claude-haiku-4-5-20251001-v1:0` |
| `family.rs:239` (inference-profile ARN) | `…/us.anthropic.claude-3-5-sonnet-20241022-v2:0` | `…/us.anthropic.claude-sonnet-4-6` |
| Conformance Bedrock (`src/eventstream.rs:118`, `tests/conformance.rs:373`) | `anthropic.claude-3-5-sonnet-20240620-v1:0` | `anthropic.claude-sonnet-4-6` |
| Conformance Anthropic `MODEL_ID` (`tests/conformance.rs:2042`) | `claude-3-5-sonnet-latest` | `claude-sonnet-4-6` |

My call: the `family.rs` cases use the Haiku 4.5 id where the old case had a
version suffix, so the `-v1:0` shape stays covered. `us-gov.` with Haiku 4.5 is
a parsing case only. It does not claim that GovCloud serves this model.

The conformance doc comment at `conformance.rs:2040-2041` cites "the task
brief". Replace it with: "Uses a current id from `KNOWN_MODELS`. The suite reads
neither the model capabilities nor the request body, so this id only has to be
one the crate accepts." (The new row changes `max_output_default` from 8192 to
32768, so the request `max_tokens` changes. `PacedServer` ignores the body, so
no result changes.)

The shorter ids let rustfmt join lines (for example `builder.rs:466-467` and
`:715-718`, `conformance.rs:372-374`, `eventstream.rs:117-119`). Run
`cargo fmt --all` after the id swap.

## Capability tests (Anthropic crate)

- `haiku_3_5_has_no_vision` goes. In its place, `haiku_4_5_has_the_smaller_output_default`
  pins properties the row really has: `max_output_default` is 8192, and
  `vision` and `prompt_caching` are `true`. It does **not** assert `reasoning`
  (see the non-goal about the Haiku 4.5 row).
- `old_3_sonnet_lacks_prompt_caching` goes. No current row lacks prompt caching,
  and `unknown_id_falls_through_to_conservative_defaults` already pins the
  all-negative case.
- My call: no test asserts that a 3.x id is absent. Verification step 1 is the
  check.

## Snapshots

The 8 Bedrock translator snapshots each contain one `"modelId"` line. The
expected change: only that line changes, from the 3.5 id to
`anthropic.claude-sonnet-4-6`. Run the snapshot tests, compare each `.snap.new`
diff with this description, and accept only a matching diff. Any other diff is
a stop: report it. No `.snap.new` file stays in the tree.

## Non-goals

- No new `KNOWN_MODELS` rows for the Claude 5 family. Today `claude-sonnet-5-5`
  gets the conservative set (`max_tokens` 4096) although the Anthropic README
  names it.
- No change to the Haiku 4.5 row. It has no `reasoning` flag, but the crate's
  own live test (`live_round_trip.rs:59-61`) uses Haiku 4.5 with extended
  thinking, so the flag is probably wrong.
- These two capability gaps go in the next providers follow-up sweep. They are
  listed in the PR body.
- No change to the LEGACY `claude-sonnet-4-5` ids that offline tests already
  use as parsing cases. They are not Claude 3.x, and they are not user-facing.
- No statement about the Anthropic use-case form for new Bedrock accounts. We
  did not observe it, and the anti-goals forbid a claim we did not observe.
- No build-time error or warning for a 3.x id. An unknown id gets the
  conservative capabilities, and the provider gives its own error.
- No change to historical files under `docs/superpowers/` or to existing
  `CHANGELOG.md` entries.
- No manual version bumps. release-plz bumps the crates and their dependents.

## Anti-goals

- A test must not change its code path. Only the id string changes.
- A snapshot must not change in any way other than the `modelId` line.
- A doc must not claim a fact that we did not observe.

## Verification

1. `git grep -nE 'claude-3|claude-instant|claude-v2' -- ':!docs/superpowers' ':!**/CHANGELOG.md'`
   returns nothing.
2. The full CI gate set from `CLAUDE.md` passes, plus `mdbook build docs/book`.
   No `.snap.new` file exists.
3. Live, negative control. Run `smoke_text_turn` only:
   `AWS_PROFILE=maip-dev-admin AWS_DEFAULT_REGION=us-east-1
   BEDROCK_MODEL_ID=anthropic.claude-3-5-sonnet-20241022-v2:0 cargo test -p
   paigasus-helikon-providers-bedrock --test live -- --ignored --nocapture smoke_text_turn`.
   It must fail, and its output must contain `end of its life` (or
   `ResourceNotFoundException`). A credential, network or compile error is a
   stop, not a pass. Put the error text in the PR body.
4. Live, example id: the same command with
   `BEDROCK_MODEL_ID=global.anthropic.claude-sonnet-5-5` and no test filter.
   Every test in the target must pass, and no `[live] … skipping` line may
   appear. On this id, every forced-choice request takes the native path. A
   failed test is a failure. If a test fails, run it once more. If it fails
   again, stop and report.
5. Anthropic API: `ANTHROPIC_API_KEY` is not set in the agent shell. **Open
   decision for Sven at the spec gate:** either export a key, and the builder
   runs `cargo test -p paigasus-helikon-providers-anthropic -- --ignored
   --nocapture` (both the `live` test target and the `live_round_trip` lib
   tests), or accept that AC4 is met on the Bedrock side only. The PR body
   records which one happened.

## Release

- Commit type `fix(providers)`, not `fix!`. `KNOWN_MODELS` is crate-private, so
  no public item changes. release-plz gives patch bumps to the Anthropic and
  Bedrock crates and to their dependent crates (`release-plz.toml:5-9`).
- Anthropic CHANGELOG upgrade note: Claude 3.x models are not supported. A
  `claude-3-*` id now reports the conservative capability set. For a
  `claude-3-5-*` id, the default `max_tokens` drops from 8192 to 4096 (Claude 3
  ids already had 4096). A user who still calls such a model can set
  `max_output_tokens` per request and `.with_capabilities()` on the builder, or
  stay on 0.3.5.
- The squash commit body repeats the upgrade note, because release-plz writes
  only commit subjects to the CHANGELOG.
- The book and the READMEs change in the same PR (user-facing).
