# SMA-832: Bedrock family detection for every geographic inference profile prefix

## Intent

`ModelFamily::from_model_id` returns `Unknown` for a `global.` inference
profile ID such as `global.anthropic.claude-sonnet-5-5`. The provider gates
structured-output synthesis and forced tool choice on the family, so these
features stop without an error. `capabilities()` also reports
`structured_output` and `parallel_tool_calls` as `false` (both are capability
flags only). The two family-gated live tests in `tests/live.rs` skip.

## Goals

- Detect the family correctly for every geographic prefix that Bedrock uses
  for system-defined inference profiles today.
- Detect the family correctly for a geographic prefix that AWS adds later,
  with no code change.
- Detect the family from a Bedrock ARN whose resource ID contains the model
  ID (foundation-model, inference-profile and prompt-router ARNs).
- Keep the result unchanged for every real ID that is detected correctly
  today.
- Give a signal (a `debug!` log) when the builder detects `Unknown`, so that
  the next misdetection is visible.

## Non-goals

- Application inference profile, provisioned-throughput and custom-model
  ARNs. Their resource IDs are opaque, so detection from the string is not
  possible. They stay `Unknown`, as today.
- A builder override such as `.family(ModelFamily)`. Sven approved this
  as follow-up issue SMA-837.
- New model families (for example DeepSeek or Qwen).
- A change to the public API. `ModelFamily` and `from_model_id` keep their
  signatures.

## Evidence: the prefixes in use

The AWS Bedrock user guide (model cards and the cross-Region inference pages,
read on 2026-11-20) shows these prefixes in real profile IDs:

| Prefix | Example ID from the AWS docs |
|---|---|
| `us.` | `us.anthropic.claude-sonnet-4-5-20250929-v1:0` |
| `eu.` | `eu.anthropic.claude-haiku-4-5-20251001-v1:0` |
| `apac.` | `apac.amazon.nova-pro-v1:0` |
| `jp.` | `jp.anthropic.claude-sonnet-4-5-20250929-v1:0` |
| `au.` | `au.anthropic.claude-sonnet-5-5` |
| `ca.` | `ca.anthropic.claude-sonnet-5-5` |
| `in.` | `in.anthropic.claude-haiku-4-5-20251001-v1:0` |
| `global.` | `global.anthropic.claude-sonnet-4-5-20250929-v1:0` |

A live `aws bedrock list-inference-profiles --type-equals SYSTEM_DEFINED`
call on 2026-11-20 confirms the same eight prefixes. It returned 168 unique
IDs from us-east-1, eu-central-1, ap-northeast-1, ap-southeast-2,
ca-central-1 and ap-south-1. Every ID has the provider in the second
segment. GovCloud (a separate partition) was not reachable from this account.

- `us-gov.`: the current docs show GovCloud source Regions on the `us.`
  profile. Older profiles may have used `us-gov.`. The design detects a
  `us-gov.` ID either way, and a test row pins this. A prefix can contain a
  hyphen, so no code may assume that it does not.
- `ap.` has no evidence in the docs. The current code strips it. The new
  design still detects `ap.`-prefixed IDs.

## Approaches

1. **Extend the prefix list.** Add `global.`, `jp.`, `au.`, `ca.` and `in.` to
   `strip_region_prefix`. This is easy to read. But a new geography from AWS
   breaks detection again, without an error. This is the same defect that
   caused this ticket.
2. **Find the provider segment (chosen by Sven).** Match the first segment
   against the known provider names. If it is not a known provider, match the
   second segment. A new geographic prefix then works with no code change. A
   provider name is never a geographic prefix, so the risk of a wrong match is
   low.

## Design

`from_model_id(model_id)`:

1. **ARN.** If the ID starts with `arn:`, replace it with the text after the
   last `/`. If the ARN has no `/`, the result is `Unknown`. So
   `arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-3-5-sonnet-20241022-v2:0`
   is classified as `anthropic.claude-3-5-sonnet-20241022-v2:0`, and an
   application inference profile ARN is classified as its opaque ID, which
   gives `Unknown`.
2. **Segments.** Split the ID on the first `.` into `first` and `rest1`. Split
   `rest1` on its first `.` into `second` and `rest2`. A missing part is the
   empty string. Compare without case sensitivity.
3. **One source of truth.** A private helper
   `classify(provider: &str, rest: &str) -> Option<ModelFamily>` returns
   `None` when `provider` is not a known provider name. For a known provider
   it returns `Some(family)`, where `family` can be `Unknown` (for example
   `amazon.<other>`). The known names are `anthropic`, `amazon`, `meta`,
   `mistral` and `cohere`. They exist only in this helper.
4. **Result.** `classify(first, rest1)`, else `classify(second, rest2)`, else
   the Llama fallback. So a known provider in the first segment always wins,
   and its result is final: `amazon.anthropic.x` is `Unknown`, not
   `Anthropic`.
5. **Llama fallback.** If `first` contains `llama`, or (`first` is not a
   provider and) `second` contains `llama`, return `Llama`. Else return
   `Unknown`. This keeps today's result for `us.llama...`-shaped IDs.

Only one prefix level is examined. Bedrock does not stack prefixes.

The private `strip_region_prefix` function is removed.

`BedrockModelBuilder::build` logs at `debug!` level with
`target: "paigasus::bedrock::builder"` when the detected family is `Unknown`.
The log includes the model ID and says that family-gated features are off.

## Behavior change for callers

A caller who uses a `global.`, `jp.`, `au.`, `ca.`, `in.` or `us-gov.` ID (or
a foundation-model or inference-profile ARN) now gets the same behavior as a
`us.` ID:

1. `ResponseFormat::JsonSchema` and `JsonObject` are sent as a forced tool
   call. Before, they became a text response.
2. `ToolChoice::Required` is sent as `toolChoice: any`, and `ToolChoice::Tool`
   as `toolChoice: tool`. Before, both were dropped.
3. These requests now fail in `build_request`, as they do for a `us.` ID
   today: a JSON format with `ToolChoice::Tool`, a JSON format with
   `ToolChoice::None`, `ToolChoice::Required` with no tools, and
   `ToolChoice::Tool` with a name that is not in the tool list.
4. `capabilities()` reports `structured_output` and `parallel_tool_calls` as
   `true`.

The PR body states these four items.

## Tests

Unit tests in `family.rs`. All current test rows stay. New rows:

| ID | Expected | Why |
|---|---|---|
| one Anthropic ID for each prefix in the evidence table | `Anthropic` | each documented prefix |
| `ap.anthropic.claude-x` | `Anthropic` | legacy prefix |
| `us-gov.anthropic.claude-3-5-sonnet-20240620-v1:0` | `Anthropic` | hyphenated prefix |
| `global.anthropic.claude-sonnet-5-5` | `Anthropic` | the ticket's ID |
| `global.amazon.nova-2-lite-v1:0` | `AmazonNova` | rest parsed after a prefix |
| `us.meta.llama3-2-90b-instruct-v1:0` | `Llama` | prefix with Llama |
| `eu.mistral.pixtral-large-2502-v1:0` | `Mistral` | prefix with Mistral |
| `zz.anthropic.claude-x` | `Anthropic` | a future prefix |
| `us.deepseek.r1-v1:0`, `us.writer.palmyra-x5-v1:0` | `Unknown` | prefix with an unknown provider |
| `amazon.anthropic.x` | `Unknown` | no fall-through after a known provider |
| `us.llama3-x` | `Llama` | Llama fallback on the second segment |
| `GLOBAL.Anthropic.claude-x`, `US.anthropic.x` | `Anthropic` | case |
| `""`, `"."`, `"global."` | `Unknown` | empty and partial input |
| `global.anthropic` | `Anthropic` | matches today's `anthropic` |
| foundation-model ARN (above) | `Anthropic` | ARN resource ID |
| `arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude-3-5-sonnet-20241022-v2:0` | `Anthropic` | inference-profile ARN |
| `arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/a1b2c3d4e5f6` | `Unknown` | opaque ARN |
| `arn:aws:bedrock` | `Unknown` | ARN with no `/` |

The doctest on `from_model_id` also asserts a `global.` ID.

Offline request-level tests (they run in CI):

- In `translate/mod.rs`, with `make_cfg("global.anthropic.claude-sonnet-5-5")`
  and `ResponseFormat::JsonSchema`: synthesis is on and the request has a
  forced `toolChoice`.
- A builder test: `converse("global.anthropic.claude-sonnet-5-5")` gives
  `capabilities().structured_output == true`.

Live acceptance: with `BEDROCK_MODEL_ID=global.anthropic.claude-sonnet-5-5`,
`tool_with_tagged_enum_and_nested_generic` and `structured_output_synthesis`
**pass** without a skip line. The PR records the output of the real run. A
skipped run does not count. Only `structured_output_synthesis` sends a request
that this fix changes; the tool test shows only that the gate opens. The live
run uses `AWS_PROFILE=maip-dev-admin`.

**Live outcome (2026-11-20, Sven's ruling).** With
`global.anthropic.claude-sonnet-5-5`, both gated tests run and do not skip.
`tool_with_tagged_enum_and_nested_generic` passes. `structured_output_synthesis`
fails, because Claude Sonnet 5.5 and Claude Opus 5.5 reject a forced
`toolChoice` ("type "tool" and "any" are not supported for this model"). The
same failure occurs on `main` with `us.anthropic.claude-sonnet-5-5`, so this
change does not cause it. Sven chose to ship SMA-832 as a detection fix and to
track the model-level defect in SMA-839. The live proof of the detection fix
is `structured_output_synthesis` passing on
`global.anthropic.claude-sonnet-4-5-20250929-v1:0` and
`global.amazon.nova-2-lite-v1:0`.

The `assistant_first_is_sent_without_placeholder` gate
(`model_id.contains("anthropic.claude")`) stays. It already works with every
prefix and with ARNs.

## Documentation

- The doc comment and doctest on `from_model_id`.
- `crates/paigasus-helikon-providers-bedrock/README.md`: line 63 (the family
  table example) and line 71 (the prefix sentence).
- `docs/book/src/concepts/model-providers.md`, lines 185-188.

Each states the rule: when the first segment is not a known provider name,
detection reads the second segment. So a geographic prefix such as `us.`,
`eu.`, `apac.`, `jp.`, `au.`, `ca.`, `in.` or `global.` is skipped, and a
future prefix is skipped too. An ARN is classified by its resource ID.

## Release

- Commit and PR type `fix(providers)`. The PR title names the new prefixes,
  so the CHANGELOG line is useful. No hand-bumped version. release-plz makes
  the patch bump and the facade cascade.
- No public API change, so `cargo-semver-checks` stays green. The behavior
  change above is a bug fix, so a patch release is correct.

## Left to the builder

- The exact wording of the documentation and the log message, within the
  rules above.
