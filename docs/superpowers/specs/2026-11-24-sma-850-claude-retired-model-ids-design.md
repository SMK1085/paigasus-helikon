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
3.x model. Every listed Claude model supports only the `INFERENCE_PROFILE`
inference type, so a bare `anthropic.…` id cannot be invoked on demand. A
current example must name a profile id (`global.…` or `us.…`).

| Id | Status |
| --- | --- |
| `anthropic.claude-sonnet-5-5`, `global.anthropic.claude-sonnet-5-5` | ACTIVE |
| `anthropic.claude-sonnet-4-6`, `us.`/`global.` profiles | ACTIVE |
| `anthropic.claude-haiku-4-5-20251001-v1:0`, `us.`/`global.` profiles | ACTIVE |
| `anthropic.claude-sonnet-4-5-20250929-v1:0` | LEGACY |
| `us.anthropic.claude-3-haiku-…`, `us.anthropic.claude-3-sonnet-…` profiles | listed ACTIVE, but the models behind them are not listed |

The only id-based behaviour rule in the Bedrock crate is
`family::rejects_forced_tool_choice` (`family.rs:147`). It matches
`claude-sonnet-5-5` and `claude-opus-5-5` only. `ForcedChoicePolicy::new`
(`policy.rs:39`) uses it to select the native output path. The Anthropic crate
uses the id only for the `KNOWN_MODELS` lookup.

## Goals

1. Every user-facing Bedrock example names `global.anthropic.claude-sonnet-5-5`
   (Sven picked this at the first question). Sites: `README.md:31`,
   `src/lib.rs:11`, `src/builder.rs:42`, `:57` and `:388` (rustdoc),
   `src/family.rs:57` (rustdoc), and `docs/book/src/concepts/model-providers.md:171`
   and `:507`.
2. The README and the book Bedrock section say in one sentence that current
   Claude models on Bedrock run through an inference profile id (`global.` or a
   regional prefix such as `us.`), and that Claude 3.x models are not supported.
3. `KNOWN_MODELS` in the Anthropic crate loses its 8 Claude 3.x and 3.5 rows and
   their two section comments. The Claude 4 rows stay unchanged.
4. Every test that uses a 3.x id as an arbitrary string uses a current 4.x id
   instead, so no test changes its code path (see "Test ids").
5. `family.rs` keeps every parsing case: base id with a version suffix, the
   `us.`, `apac.` and `us-gov.` prefixes, the foundation-model ARN and the
   inference-profile ARN. Only the model part of each id changes.
6. The Anthropic crate CHANGELOG gets an `### Upgrade notes` entry under
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
brief". Replace it with: "Uses a current id from `KNOWN_MODELS`. The mock
server ignores the request, so this id only has to be one the crate accepts."
`claude-sonnet-4-6` has a superset of the old row's capability flags (it adds
`reasoning`), so no conformance check that reads capabilities loses a flag.
The builder confirms this when it runs the suite.

## Capability tests (Anthropic crate)

- `haiku_3_5_has_no_vision` goes. In its place, `haiku_4_5_has_no_reasoning`
  pins a negative flag on a current row: `reasoning` is `false`, and
  `prompt_caching` and `vision` are `true`, `max_output_default` is 8192.
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

- No new `KNOWN_MODELS` rows for the Claude 5 family. That is a separate gap.
- No change to the LEGACY `claude-sonnet-4-5` ids that tests already use. They
  are not Claude 3.x.
- No build-time error or warning for a 3.x id. An unknown id gets the
  conservative capabilities, and the provider gives its own error.
- No change to historical files under `docs/superpowers/` or to existing
  `CHANGELOG.md` entries.
- No manual version bumps. release-plz bumps the crates and the facade.

## Anti-goals

- A test must not change its code path. Only the id string changes.
- A snapshot must not change in any way other than the `modelId` line.

## Verification

1. `git grep -nE 'claude-3|claude-instant|claude-v2' -- ':!docs/superpowers' ':!**/CHANGELOG.md'`
   returns nothing.
2. The full CI gate set from `CLAUDE.md` passes, plus `mdbook build docs/book`.
   No `.snap.new` file exists.
3. Live, negative control: `AWS_PROFILE=maip-dev-admin AWS_DEFAULT_REGION=us-east-1
   BEDROCK_MODEL_ID=anthropic.claude-3-5-sonnet-20241022-v2:0 cargo test -p
   paigasus-helikon-providers-bedrock --test live -- --ignored --nocapture`
   must **fail** with a service error. This proves that the run reached Bedrock.
4. Live, example id: the same command with
   `BEDROCK_MODEL_ID=global.anthropic.claude-sonnet-5-5` must pass. A test that
   needs a forced tool choice may take the native path on this model; that is
   expected behaviour, not a failure.
5. Anthropic API: `ANTHROPIC_API_KEY` is not set in the agent shell. If Sven
   exports one, run `cargo test -p paigasus-helikon-providers-anthropic --test live
   -- --ignored`. Otherwise the PR body says that the Anthropic API side was not
   live-tested in this PR. No user-facing Anthropic example changes in this PR.

## Release

- Commit type `fix(providers)`, not `fix!`. `KNOWN_MODELS` is crate-private, so
  no public item changes. release-plz gives patch bumps to the Anthropic and
  Bedrock crates and cascades to the facade.
- Anthropic CHANGELOG upgrade note: Claude 3.x models are not supported. A
  `claude-3-*` id now reports the conservative capability set, and the default
  `max_tokens` for such an id becomes 4096.
- The book and the Bedrock README change in the same PR (user-facing).
