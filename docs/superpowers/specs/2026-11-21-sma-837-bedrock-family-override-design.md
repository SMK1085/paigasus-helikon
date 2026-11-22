# SMA-837: Bedrock builder family override for opaque model IDs

Status: approved 2026-11-21. Size: a small change (one crate, one new builder method, docs).

## Intent

The Bedrock provider detects the model family from the model ID string. Some
model IDs are opaque: application inference profile ARNs
(`arn:aws:bedrock:<region>:<acct>:application-inference-profile/<id>`),
provisioned-throughput ARNs and custom-model ARNs. For these IDs the detected
family is always `ModelFamily::Unknown`.

`build_request` reads the family, not the capabilities. So for an opaque ID,
structured-output synthesis and forced tool choice are off.
`BedrockModelBuilder::capabilities(...)` changes only what `capabilities()`
reports. A user of an opaque ID has no way to turn on these features. This
change gives that user a way.

## Goals

- Add `BedrockModelBuilder::family(ModelFamily) -> Self`. When it is set, the
  builder uses that family and does not detect one from the model ID.
- The override has the same effect as a detected family on all three family
  consumers:
  1. `Config.family`, which the request translation reads: `translate/mod.rs`
     (structured-output synthesis, conflict guards, forced `toolChoice`) and
     `translate/native.rs` (the anchor tool). `Ruleset::for_family` ignores
     its argument today. Both files read the same field, so do not add a
     family parameter to one path only.
  2. The default capabilities (`caps_for(family)`).
  3. `ForcedChoicePolicy.family`, which the SMA-839 rule-B fallback reads
     (`fallback.rs` checks `policy.family() == ModelFamily::Anthropic`).
- Without the override, the behavior is unchanged.
- Document the method in the rustdoc, the crate README ("Model families") and
  the mdBook Bedrock section (`docs/book/src/concepts/model-providers.md`,
  "Family-gated structured output"). The examples use
  `BedrockModel::converse(arn).sdk_config(&sdk_cfg).family(ModelFamily::Anthropic).build()?`,
  because `from_env` gives no place to set the override. The mdBook uses the
  facade path `paigasus_helikon::bedrock::ModelFamily`.
- Correct the existing text that points users to `.capabilities(...)` or says
  that the family is always detected:
  - the module doc in `capabilities.rs:8-9`;
  - the `.capabilities()` rustdoc (add: it changes only what `capabilities()`
    reports, and does not change the request; link to `.family()`);
  - the `Config.family` field doc ("Detected model family");
  - the `ForcedChoicePolicy::new` doc;
  - the `ModelFamily` type doc ("Used internally");
  - the "model family not detected" `debug!` text: add "set
    BedrockModelBuilder::family to choose the family".
- The rustdoc and README say that the override has the full effect of a
  detected family, also on the caller's tool choice. On an opaque ID today,
  `ToolChoice::Required` and `ToolChoice::Tool` are omitted. With
  `.family(Anthropic)` they become `toolChoice: any` and a named tool choice.
  These requests become local errors: `JsonSchema` or `JsonObject` with
  `ToolChoice::Tool` or `ToolChoice::None`, `ToolChoice::Required` with no
  tools, and `ToolChoice::Tool` with an undefined tool name.

## Non-goals

- No new detection rules for opaque IDs. An opaque ID contains no family.
- No change to `ModelFamily` (it is already public and `#[non_exhaustive]`;
  callers can name its variants).
- No AWS call to resolve an application inference profile to its model. The
  builder stays synchronous and offline.
- No hand version bump. release-plz gives the additive change a patch bump.
- No `FromStr` for `ModelFamily` and no `BedrockModel::family()` accessor.
  Both are possible follow-ups.
- No opt-in to the native path from the first request on an opaque ID (for
  example a builder input for the underlying model ID). Decision
  (2026-11-21): no follow-up ticket.

## Anti-goals

- The override must not split the family: the request translation, the
  capabilities and the policy must never see different families.
- `.capabilities(...)` must keep its current meaning and precedence.

## Design

1. `BedrockModelBuilder` gets a field `family_override: Option<ModelFamily>`,
   default `None`.
2. `family(mut self, f: ModelFamily) -> Self` sets it. A second call replaces
   the first value, the same as the other builder setters.
3. In `build()`, step 3 becomes:
   `let family = self.family_override.unwrap_or_else(|| ModelFamily::from_model_id(&self.model_id));`
   Put this resolution in one function that `build()` and the test helper
   `offline_config` both call, so the test helper cannot drift from `build()`.
   The existing "model family not detected" debug log fires only when no
   override is set and detection gives `Unknown`. When an override is set:
   - if detection gives a known family (not `Unknown`) and the override is a
     different family that is not `Unknown`, log a `warn!` that names both
     families and the model ID (this is almost always a mistake);
   - otherwise log one `debug!` that names the override family and the model
     ID.
4. `caps_for(family)` and `ForcedChoicePolicy::new(&model_id, family)` receive
   the effective family. So they agree with `Config.family`.
5. Capability precedence: `.capabilities(...)` > `caps_for(override family)` >
   `caps_for(detected family)`.
6. `.family(ModelFamily::Unknown)` is valid. It turns the family-gated
   behavior off for an ID that detection would classify.

### Static rule A with an override (my calls)

`rejects_forced_tool_choice(model_id)` reads only the model ID, and it is
`true` only when the detected family is `Anthropic` and the ID names Claude
Sonnet 5.5 or Opus 5.5. Two consequences:

- Opaque ID plus `.family(Anthropic)`: rule A is off, because the ID does not
  name the model. The first structured-output request sends a forced
  `toolChoice`. If the profile points at Sonnet 5.5 or Opus 5.5, Bedrock
  rejects it once, and rule B moves the handle to native output (this works
  because the policy carries the override family). The cost is one rejected
  request per handle, plus the costs below. Document all of them in the
  README and the mdBook, and tell users to build one handle and clone it:
  - Before rule B learns, some requests fail locally on the forced path and
    never reach Bedrock (`translate/mod.rs:196-214`): `JsonSchema`/`JsonObject`
    with `ToolChoice::Tool`, and `JsonSchema` with `ToolChoice::None`. These
    requests cannot teach the handle. The same requests succeed on the native
    path. So the result depends on the order of the requests.
  - Several first requests in flight on one handle at the same time each get
    a rejection, because the learned flag is set only after a rejection comes
    back.
  - A handle that is made again for each run pays the cost again.
- The recovery needs rule B, and rule B matches two literal rejection texts
  (`error.rs:80-84`). Nothing yet shows that Bedrock returns the same text,
  and treats `toolChoice` and `outputConfig` the same, for an application
  inference profile ARN as for the plain model ID. Add an env-gated
  `#[ignore]` live test (`BEDROCK_PROFILE_ARN`) that builds with
  `converse(arn).sdk_config(..).family(ModelFamily::Anthropic)`, sends a
  `JsonSchema` request, asserts valid JSON output, and reports
  `policy.path()`. Decision (2026-11-21): the dev AWS account has no
  application inference profile, so nobody runs this test before merge. The
  docs say that the rule-B recovery for profiles is not verified live.
- Rule A gate. `ForcedChoicePolicy::new` sets the static rule only when the
  effective family supports forced tool choice:
  `rejects_static = family.supports_forced_tool_choice() && rejects_forced_tool_choice(model_id)`.
  Reason: with `.family(Unknown)` (or Llama, Titan, Cohere) on a Sonnet 5.5
  ID, an ungated rule A selects the native path, and `build_native` sends
  `outputConfig` with no family check (`translate/native.rs:59-61`). That
  breaks goal 6. With this gate, a Mistral or Nova override on a Sonnet 5.5 ID
  goes to the native path, which works on the real model. (A gate on
  `family == Anthropic` would send a forced choice to a model that rejects
  it, and rule B, which is for Anthropic only, would never recover.)
  Without an override this is the same value as today, because
  `rejects_forced_tool_choice` is already `false` for a non-Anthropic detected
  family.

## Approaches considered

- **A. Builder method `family(ModelFamily)` that feeds all consumers (chosen).**
  The ticket names this design. It is one field and one resolved value.
- **B. Make `.capabilities(...)` turn on the family-gated behavior.** Rejected.
  Capabilities do not carry a family, so the translation cannot pick a schema
  ruleset or a forced-choice rule from them. It also changes the meaning of an
  existing method.
- **C. A model-ID-to-family map on the builder or a global registry.** Rejected.
  It is more surface for the same result, and a global registry is hidden
  state.

## Tests

Every test below builds the model through the public builder:
`BedrockModel::converse(id).client(offline_client()).family(..).build()`. It
then calls `build_request(&model.config, &req, model.policy.path())` and
`to_wire_json`. It does not make `Config` or the policy by hand, because that
would repeat the wiring under test. Each test asserts
`model.config.family == model.policy.family()` and that
`model.capabilities().structured_output` agrees with that family.

- Acceptance: an application inference profile ARN built with
  `.family(ModelFamily::Anthropic)`, and a `ResponseFormat::JsonSchema`
  request, gives a wire request with the synthesized structured-output tool
  and a forced `toolChoice` (`{ tool: { name } }`).
- Acceptance: the same ARN without the override gives no synthesized tool and
  no `toolChoice` (unchanged behavior).
- On the ARN with the override, `ToolChoice::Required` gives `{ any: {} }`.
- `.capabilities(...)` still wins over the override defaults.
- `.family(ModelFamily::Unknown)` on a detectable Anthropic ID turns synthesis
  off.
- `global.anthropic.claude-sonnet-5-5` with `.family(ModelFamily::Unknown)`
  and a `JsonSchema` request gives no `outputConfig`, no `toolConfig`, and
  `structured_output == false`.
- `.family(ModelFamily::Anthropic)` on `global.anthropic.claude-sonnet-5-5`
  still selects the native path (an override equal to the detected family
  keeps rule A on).
- `global.anthropic.claude-sonnet-5-5` with `.family(ModelFamily::Mistral)`
  selects the native path (rule A gate above).
- Rule B through a built handle: the `drive` helper in `fallback.rs` with
  `&model.policy` and `&model.config` from the ARN with `.family(Anthropic)`
  and the script `[Err(rejected()), Ok(())]` gives a forced request, then a
  native request. Control: the same ARN without the override sends one
  request and no fallback.
- The env-gated live test for a profile ARN (see above).

## Final sweep

After the implementation, search the whole repository (code, rustdoc,
READMEs, mdBook, runbooks) for other text that this change makes stale. For
example, text that says an opaque ID cannot get structured output, or that
points users to `.capabilities(...)` for that purpose. Correct each hit on
this branch.

## Left to the builder

- The exact test placement (`builder.rs` tests and `translate/mod.rs` tests
  both have offline helpers; `offline_config` can gain an override variant).
- The exact README and mdBook wording, in Simplified Technical English.
